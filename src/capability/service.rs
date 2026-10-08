use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::app::status::local_worker_snapshot;
use crate::app::system::{
    cancel_workspace_build, inspect_project_workspace, launch_project_workspace,
    launch_remote_connection, launch_workspace_build, local_agent_executable_metadata, open_folder,
    signed_agent_updates_required, trusted_agent_update_key_ids, workspace_build_status,
};
use crate::app::types::{ProjectWorkspaceRequest, RemoteConnectRequest, WorkspaceBuildRequest};
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
use crate::capability::execution::CapabilityExecutionContext;
use crate::capability::plugin::{
    find_plugin, invoke_plugin_capability, invoke_plugin_capability_for_plugin,
    registry_json_for_control_plane, scan_plugins,
};
use crate::capability::software_distribution::{
    attach_inspection_receipt, consume_inspection_receipt, verify_inspection_receipt,
};
use crate::capability::types::{
    CapabilityAvailability, CapabilityDescriptor, InvocationContext, InvocationTransport,
};
use crate::extension_contracts::DistributionTarget;
use crate::store::credentials::{local_login_status_json, local_login_status_value};
use crate::store::types::LocalWorkerStatus;
use crate::svn::service::{
    checkout_workspace, create_exhibit_repository_path, create_repository_with_post_commit_hook,
    ensure_project_exhibits_access, initialize_exhibit_repository_with_cancel, list_connections,
    open_workspace, scan_migration_source, test_connection, update_workspace, workspace_status,
};
use crate::svn::types::{
    CreateExhibitRepositoryPathRequest, CreateRepositoryRequest,
    EnsureProjectExhibitsAccessRequest, InitializeExhibitRepositoryRequest,
    MigrationSourceScanRequest, SvnCheckoutRequest, SvnWorkspaceRequest,
};
use crate::{Options, VERSION};

const REGISTRY_CACHE_TTL: Duration = Duration::from_secs(5);

/// Bumped whenever the discoverable capability set changes.
///
/// The registry cache lives on the Gateway instance, while the mutating call
/// sites (plugin install/uninstall/rollback, MCP server add/remove, extension
/// source install) are free functions without access to it.  A process-wide
/// epoch lets those paths announce the change so the next capability listing
/// rebuilds instead of waiting out `REGISTRY_CACHE_TTL`.
static CAPABILITY_DISCOVERY_EPOCH: AtomicU64 = AtomicU64::new(1);

pub(crate) fn invalidate_capability_discovery() {
    CAPABILITY_DISCOVERY_EPOCH.fetch_add(1, Ordering::AcqRel);
}

#[derive(Clone)]
pub(crate) struct CapabilityGateway {
    options: Options,
    worker_status: Arc<Mutex<LocalWorkerStatus>>,
    approval_manager: Arc<ApprovalManager>,
    downstream_mcp: DownstreamMcpManager,
    business_provider: Arc<dyn BusinessIntegrationProvider>,
    registry_cache: Arc<Mutex<RegistryCache>>,
}

#[derive(Default)]
struct RegistryCache {
    revision: u64,
    epoch: u64,
    refreshed_at: Option<Instant>,
    registry: Option<BTreeMap<String, CapabilityRegistration>>,
}

#[derive(Clone)]
enum CapabilityHandler {
    SystemHealth,
    EngineeringProjectResolve,
    EngineeringCheckpointCreate,
    EngineeringWorkspaceLeaseAcquire,
    EngineeringWorkspaceLeaseRelease,
    EngineeringWorkspaceLeaseList,
    EngineeringHandoffCreate,
    WorkflowCatalogList,
    WorkflowCatalogDescribe,
    WorkflowRunStart,
    WorkflowRunGet,
    ScheduleList,
    ScheduleSet,
    ScheduleDelete,
    WorkflowPresetList,
    WorkflowPresetSet,
    WorkflowPresetDelete,
    SkillRunStart,
    SkillRunList,
    SkillRunGet,
    WorkflowRunFeedback,
    WorkflowRunCancel,
    WorkflowCandidateFreeze,
    CapabilityCatalogSearch,
    CapabilityCatalogDescribe,
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
    ExtensionDistributionTargetGet,
    ExtensionDistributionTargetSet,
    ExtensionDistributionPreview,
    ExtensionDistributionPublish,
    ExtensionDistributionStateGet,
    ReleaseInstallPlan,
    ReleaseInstallApply,
    GithubAccountGet,
    GithubAccountSet,
    GithubAccountRemove,
    GithubAppAuthorizeStart,
    GithubAppAuthorizePoll,
    GithubAppInstallations,
    GithubAppInstallationSelect,
    InnerAdminLoginStatus,
    SystemOpenFolder,
    FilesystemDelete,
    WorkspaceBuild,
    WorkspaceBuildStatus,
    WorkspaceBuildCancel,
    WorkspaceStatus,
    WorkspaceOpen,
    RemoteConnect,
    ScanProjects,
    InnerAdminSyncExhibits,
    UploadCode,
    UploadPlaceholder,
    SmbUpload,
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
    SvnExhibitRepositoryClone,
    SvnExhibitRepositoryImportLocal,
    SvnProjectExhibitsAccessEnsure,
    SvnProjectAclPreview,
    SvnProjectAclApply,
    SvnProjectAclReconcile,
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
    SkillCandidateConfirm,
    PluginCandidateSave,
    PluginCandidateTest,
    PluginCandidateConfirm,
    WorkflowCandidateSave,
    WorkflowCandidateTest,
    WorkflowCandidateConfirm,
    WorkflowSubmissionSubmit,
    WorkflowSubmissionStatus,
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
    MarketSearch,
    MarketInstalled,
    MarketInstallPlan,
    MarketInstall,
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
            registry_cache: Arc::new(Mutex::new(RegistryCache::default())),
            options,
            worker_status,
            approval_manager,
        }
    }

    pub(crate) fn options(&self) -> &Options {
        &self.options
    }

    #[cfg(test)]
    pub(crate) fn replace_business_catalog_for_test(&self, snapshot: BusinessCatalogSnapshot) {
        let provider = self
            .business_provider
            .as_any()
            .downcast_ref::<DashboardCatalogProvider>()
            .expect("test Gateway must use the Dashboard business integration provider");
        provider.replace_snapshot(snapshot);
        self.invalidate_registry_cache();
    }

    #[cfg(test)]
    fn invalidate_registry_cache(&self) {
        if let Ok(mut cache) = self.registry_cache.lock() {
            cache.revision = cache.revision.wrapping_add(1);
            cache.refreshed_at = None;
            cache.registry = None;
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
                if is_svn_admin_capability(&registration.descriptor.id) {
                    return false;
                }
                if is_task_scoped_capability(&registration.descriptor.id)
                    && context.source != crate::capability::types::InvocationSource::DashboardWorker
                {
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
            .map(|registration| {
                let mut descriptor = registration.descriptor;
                annotate_business_exhibit_id_contract(&mut descriptor);
                descriptor
            })
            .collect())
    }

    pub(crate) fn search_capabilities(
        &self,
        context: &InvocationContext,
        params: &Value,
    ) -> Result<Value, Box<dyn Error>> {
        let query = params
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let group = params
            .get("group")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let surface = params
            .get("surface")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let availability = params
            .get("availability")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let source = params
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let limit = params
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .clamp(1, 100) as usize;
        let all_capabilities = self.list_capabilities(context)?;
        let generation = capability_catalog_generation(&all_capabilities);
        let filter_generation =
            capability_catalog_filter_generation(&query, &group, &surface, &availability, &source);
        let offset = parse_capability_catalog_cursor(
            params.get("cursor").and_then(Value::as_str),
            &generation,
            &filter_generation,
        )?;
        let mut capabilities = all_capabilities;
        capabilities.retain(|capability| {
            let searchable = format!(
                "{} {} {} {}",
                capability.id, capability.name, capability.description, capability.source
            )
            .to_ascii_lowercase();
            (query.is_empty() || searchable.contains(&query))
                && (group.is_empty() || capability.discovery_group() == group)
                && (surface.is_empty() || capability.discovery_surface() == surface)
                && (availability.is_empty() || capability.availability.as_str() == availability)
                && (source.is_empty() || capability.source.to_ascii_lowercase().contains(&source))
        });
        if offset > capabilities.len() {
            return Err("invalid capability catalog cursor".into());
        }
        let end = offset.saturating_add(limit).min(capabilities.len());
        let items = capabilities[offset..end]
            .iter()
            .map(|capability| {
                let schema_bytes = serde_json::to_vec(&capability.input_schema)
                    .map(|schema| schema.len())
                    .unwrap_or_default();
                json!({
                    "id": capability.id,
                    "name": capability.name,
                    "description": capability.description,
                    "version": capability.version,
                    "source": capability.source,
                    "contractSource": capability.contract_source,
                    "contractGeneration": capability.contract_generation,
                    "availability": capability.availability,
                    "riskLevel": capability.risk_level,
                    "approvalRequired": capability.approval_required,
                    "executionMode": capability.execution_mode,
                    "supportsProgress": capability.supports_progress,
                    "supportsCancel": capability.supports_cancel,
                    "requiredScope": capability.required_scope,
                    "dashboardRoute": capability.dashboard_route,
                    "discoveryGroup": capability.discovery_group(),
                    "discoverySurface": capability.discovery_surface(),
                    "schemaBytes": schema_bytes,
                    "schemaAvailable": true
                })
            })
            .collect::<Vec<_>>();
        let mut result = json!({
            "items": items,
            "total": capabilities.len(),
            "capabilityGeneration": generation
        });
        if end < capabilities.len() {
            result["nextCursor"] = json!(format_capability_catalog_cursor(
                result["capabilityGeneration"].as_str().unwrap_or_default(),
                &filter_generation,
                end
            ));
        }
        Ok(result)
    }

    pub(crate) fn describe_capability(
        &self,
        context: &InvocationContext,
        params: &Value,
    ) -> Result<Value, Box<dyn Error>> {
        let id = params
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or("capability.catalog.describe requires a non-empty id")?;
        let capability = self
            .list_capabilities(context)?
            .into_iter()
            .find(|capability| capability.id == id)
            .ok_or_else(|| format!("capability not found: {id}"))?;
        Ok(json!({
            "id": capability.id,
            "name": capability.name,
            "description": capability.description,
            "version": capability.version,
            "source": capability.source,
            "contractSource": capability.contract_source,
            "contractGeneration": capability.contract_generation,
            "availability": capability.availability,
            "riskLevel": capability.risk_level,
            "approvalRequired": capability.approval_required,
            "executionMode": capability.execution_mode,
            "supportsProgress": capability.supports_progress,
            "supportsCancel": capability.supports_cancel,
            "idempotency": capability.idempotency,
            "retryPolicy": capability.retry_policy,
            "concurrency": capability.concurrency,
            "requiredScope": capability.required_scope,
            "dashboardRoute": capability.dashboard_route,
            "discoveryGroup": capability.discovery_group(),
            "discoverySurface": capability.discovery_surface(),
            "inputSchema": capability.input_schema
        }))
    }

    fn registry(&self) -> Result<BTreeMap<String, CapabilityRegistration>, Box<dyn Error>> {
        loop {
            let revision = {
                let cache = self
                    .registry_cache
                    .lock()
                    .map_err(|_| "capability registry cache lock poisoned")?;
                // The discovery epoch is process-wide, so in a test binary every
                // other test that mutates plugins or MCP servers would knock
                // this cache out from under the test currently running.  Tests
                // therefore keep the plain TTL behaviour and assert the epoch
                // contract directly instead.
                #[cfg(not(test))]
                let epoch_current =
                    cache.epoch == CAPABILITY_DISCOVERY_EPOCH.load(Ordering::Acquire);
                #[cfg(test)]
                let epoch_current = true;
                if cache
                    .refreshed_at
                    .is_some_and(|refreshed_at| refreshed_at.elapsed() < REGISTRY_CACHE_TTL)
                    && epoch_current
                {
                    if let Some(registry) = cache.registry.as_ref() {
                        return Ok(registry.clone());
                    }
                }
                cache.revision
            };

            // Registry construction scans local extension state and can be
            // slow. Never hold the cache lock during that work. A revision
            // guard prevents an older concurrent build from overwriting a
            // newer invalidated catalog.
            let registry = self.build_registry()?;
            let mut cache = self
                .registry_cache
                .lock()
                .map_err(|_| "capability registry cache lock poisoned")?;
            if cache.revision != revision {
                continue;
            }
            cache.epoch = CAPABILITY_DISCOVERY_EPOCH.load(Ordering::Acquire);
            cache.refreshed_at = Some(Instant::now());
            cache.registry = Some(registry.clone());
            return Ok(registry);
        }
    }

    fn cached_capability_count(&self) -> Option<usize> {
        self.registry_cache
            .lock()
            .ok()
            .and_then(|cache| cache.registry.as_ref().map(BTreeMap::len))
    }

    fn build_registry(&self) -> Result<BTreeMap<String, CapabilityRegistration>, Box<dyn Error>> {
        let mut registry = BTreeMap::new();
        let ai_client_targets = crate::app::ai_provider_import::known_adapter_ids();
        let builtins = [
            registration(
                "capability.catalog.search",
                "能力目录搜索",
                "按关键字、分组、surface、可用性和来源搜索当前可见能力，仅返回轻量摘要。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "maxLength": 2000 },
                        "group": { "type": "string" },
                        "surface": { "type": "string" },
                        "availability": { "type": "string", "enum": ["local", "network_service", "control_plane"] },
                        "source": { "type": "string" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
                        "cursor": { "type": "string" }
                    },
                    "additionalProperties": false
                }),
                CapabilityHandler::CapabilityCatalogSearch,
            ),
            registration(
                "capability.catalog.describe",
                "能力详情",
                "按稳定能力 ID 返回能力契约和完整输入 Schema。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": { "id": { "type": "string", "minLength": 1 } },
                    "required": ["id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::CapabilityCatalogDescribe,
            ),
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
                "为指定 AI 客户端配置指定 AI 服务；执行前应确认目标客户端、服务源和本机配置变更。目标已注册其它来源时默认拒绝，replace=true 表示先撤销旧注册再写入。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "target": { "type": "string", "enum": ai_client_targets.clone() },
                        "service": {
                            "type": "string",
                            "default": "managed",
                            "pattern": "^(managed|custom:[A-Za-z0-9_-]{1,64})$"
                        },
                        "replace": {
                            "type": "boolean",
                            "default": false,
                            "description": "目标客户端已注册其它 AI 服务时，先撤销旧注册再写入当前服务"
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
                        "protocol": { "type": "string", "enum": ["openai-chat", "openai-responses", "anthropic"] },
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
                        "kind": { "type": "string", "enum": ["plugin", "skill", "workflow"] },
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
                "返回当前扩展工作区、绑定来源，以及检测到的插件或 Skill 项目身份。可传入 workspace_root 查询指定会话的工作区。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": { "workspace_root": { "type": "string" } },
                    "additionalProperties": false
                }),
                CapabilityHandler::ExtensionWorkspaceCurrent,
            ),
            registration(
                "extension.workspace.bind",
                "绑定扩展工作区",
                "将外部 AI 会话绑定到聚合仓库、插件或 Skill 目录；绑定按会话累加，互不覆盖。",
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
                "清除 Agent 保存的外部 AI 扩展工作区绑定；传入 workspace_root 只解除该目录，否则清除本机全部绑定。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": { "workspace_root": { "type": "string" } },
                    "additionalProperties": false
                }),
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
                "extension.distribution.target.get",
                "读取扩展分发目标",
                "读取扩展项目的生效分发目标（工作台 / GitHub / 两者）及其来源：项目覆盖、分发单元默认或系统默认。不传 kind 和 id 时返回全部项目。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["plugin", "skill", "workflow"] },
                        "id": { "type": "string" }
                    },
                    "additionalProperties": false
                }),
                CapabilityHandler::ExtensionDistributionTargetGet,
            ),
            registration(
                "extension.distribution.target.set",
                "设置扩展分发目标",
                "设置扩展项目的分发目标覆盖。传 targets 为生效集合，传 inherit=true 清除覆盖并回到分发单元默认。空集合会被拒绝。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["plugin", "skill", "workflow"] },
                        "id": { "type": "string", "minLength": 1 },
                        "targets": {
                            "type": "array",
                            "minItems": 1,
                            "items": { "type": "string", "enum": ["workbench", "github"] }
                        },
                        "inherit": { "type": "boolean" }
                    },
                    "required": ["kind", "id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::ExtensionDistributionTargetSet,
            ),
            registration(
                "extension.distribution.preview",
                "预览扩展发布",
                "在不产生任何远端副作用的前提下，返回这次发布的目标、仓库、tag、资产名、制品摘要与凭据状态。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["plugin", "skill", "workflow"] },
                        "id": { "type": "string" },
                        "version": { "type": "string" }
                    },
                    "required": ["kind", "id", "version"],
                    "additionalProperties": false
                }),
                CapabilityHandler::ExtensionDistributionPreview,
            ),
            registration(
                "extension.distribution.publish",
                "按目标发布扩展",
                "按项目生效的分发目标投递已确认候选制品：先 GitHub Release（tag + 制品 + 发布清单），再工作台提审。全部成功为 released，部分成功为 partially_published，失败写入分发台账。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["plugin", "skill", "workflow"] },
                        "id": { "type": "string" },
                        "version": { "type": "string" }
                    },
                    "required": ["kind", "id", "version"],
                    "additionalProperties": false
                }),
                CapabilityHandler::ExtensionDistributionPublish,
            ),
            registration(
                "extension.distribution.state.get",
                "读取扩展分发台账",
                "读取扩展制品的分发台账：每个版本在各目标上的状态、tag、Release 地址、制品摘要与错误信息。不传 kind 和 id 时返回全部记录。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["plugin", "skill", "workflow"] },
                        "id": { "type": "string" }
                    },
                    "additionalProperties": false
                }),
                CapabilityHandler::ExtensionDistributionStateGet,
            ),
            registration(
                "github.account.get",
                "读取 GitHub 账号",
                "读取本机保存的 GitHub 分发账号状态（登录名与已授权仓库）。不返回令牌内容。",
                "read_only",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::GithubAccountGet,
            ),
            registration(
                "github.account.set",
                "授权 GitHub 账号",
                "校验并保存 GitHub 分发凭据：先用令牌调用 GitHub 校验登录名，再以 DPAPI 加密存储。令牌不会写入日志或项目记录。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "token": { "type": "string", "minLength": 1 },
                        "token_kind": { "type": "string", "enum": ["fine_grained_pat", "classic_pat"] },
                        "repositories": {
                            "type": "array",
                            "items": { "type": "string" }
                        }
                    },
                    "required": ["token"],
                    "additionalProperties": false
                }),
                CapabilityHandler::GithubAccountSet,
            ),
            registration(
                "github.account.remove",
                "解除 GitHub 授权",
                "删除本机保存的 GitHub 分发凭据。",
                "local_write",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::GithubAccountRemove,
            ),
            registration(
                "github.app.authorize.start",
                "开始 GitHub App 授权",
                "申请 GitHub App 设备码，返回 user_code 与验证地址，用户在浏览器完成授权后用 github.app.authorize.poll 继续。需要组织先注册 App 并配置 HIMIND_GITHUB_APP_CLIENT_ID。",
                "local_write",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::GithubAppAuthorizeStart,
            ),
            registration(
                "github.app.authorize.poll",
                "轮询 GitHub App 授权",
                "用设备码换取 user token；返回 pending / slow_down 时按 interval 再次调用。授权成功会保存授权事实并返回可用安装列表。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": { "device_code": { "type": "string", "minLength": 1 } },
                    "required": ["device_code"],
                    "additionalProperties": false
                }),
                CapabilityHandler::GithubAppAuthorizePoll,
            ),
            registration(
                "github.app.installations",
                "列出 GitHub App 安装",
                "列出当前用户可用的 GitHub App 安装（组织或个人），供绑定发布目标。",
                "read_only",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::GithubAppInstallations,
            ),
            registration(
                "github.app.installation.select",
                "绑定 GitHub App 安装",
                "绑定选定的安装；之后的发布、安装与私仓读取都使用该安装签发的短期令牌。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": { "installation_id": { "type": "string", "minLength": 1 } },
                    "required": ["installation_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::GithubAppInstallationSelect,
            ),
            registration(
               "extension.distribution.install.plan",
                "解析扩展安装计划",
                "读取 GitHub Release 的发布清单，按依赖优先的拓扑序返回安装计划。会在本机验证依赖是否已安装、pin 是否与清单一致，不写入任何安装目录。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "repository": { "type": "string", "minLength": 1 },
                        "tag": { "type": "string", "minLength": 1 },
                        "id": { "type": "string", "minLength": 1 },
                        "version": { "type": "string", "minLength": 1 }
                    },
                    "required": ["repository", "tag", "id", "version"],
                    "additionalProperties": false
                }),
                CapabilityHandler::ReleaseInstallPlan,
            ),
            registration(
                "extension.distribution.install",
                "从 Release 安装扩展",
                "按发布清单安装扩展：逐个下载并校验制品摘要，依赖优先安装，任一环节失败整体回滚。dry_run=true 只下载与校验，不写入安装目录。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "repository": { "type": "string", "minLength": 1 },
                        "tag": { "type": "string", "minLength": 1 },
                        "id": { "type": "string", "minLength": 1 },
                        "version": { "type": "string", "minLength": 1 },
                        "dry_run": { "type": "boolean" }
                    },
                    "required": ["repository", "tag", "id", "version"],
                    "additionalProperties": false
                }),
                CapabilityHandler::ReleaseInstallApply,
            ),
            registration(
                "market.search",
                "市场能力搜索",
                "在扩展市场里搜索可安装的技能、插件与工作流。缺能力时先用它找候选，再判断是否要装。只读，不改动本机。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["skill", "plugin", "workflow"] },
                        "query": { "type": "string", "maxLength": 200 },
                        "category": { "type": "string", "maxLength": 120 },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
                        "cursor": { "type": "integer", "minimum": 0, "default": 0 },
                        "source": { "type": "string" }
                    },
                    "additionalProperties": false
                }),
                CapabilityHandler::MarketSearch,
            ),
            registration(
                "market.installed",
                "已安装能力盘点",
                "盘点本机已经拥有的技能、插件与工作流：技能带安装落点，插件带运行态，工作流带启停与来源。只读。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["skill", "plugin", "workflow"] }
                    },
                    "additionalProperties": false
                }),
                CapabilityHandler::MarketInstalled,
            ),
            registration(
                "market.install.plan",
                "市场安装计划",
                "在真正安装之前先给出计划：版本、来源、制品摘要、依赖、安装落点与阻塞原因。dry_run 只算是计划的一部分，本能力本身不写入任何文件。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["skill", "plugin", "workflow"] },
                        "id": { "type": "string", "minLength": 1, "maxLength": 200 },
                        "version": { "type": "string" },
                        "source": { "type": "string" },
                        "artifact_id": { "type": "string" },
                        "sha256": { "type": "string" },
                        "workspace_root": { "type": "string" },
                        "target_clients": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["kind", "id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::MarketInstallPlan,
            ),
            registration(
                "market.install",
                "从市场安装能力",
                "把市场里的技能、插件或工作流装到本机。执行前会重新算一遍计划，计划未就绪时拒绝执行；技能可装到全局或指定项目目录，并可指定投放的 AI 客户端。这是写入本机的操作，须由用户确认后执行，模型不得替用户批准。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["skill", "plugin", "workflow"] },
                        "id": { "type": "string", "minLength": 1, "maxLength": 200 },
                        "version": { "type": "string" },
                        "source": { "type": "string" },
                        "artifact_id": { "type": "string" },
                        "sha256": { "type": "string" },
                        "workspace_root": { "type": "string" },
                        "target_clients": { "type": "array", "items": { "type": "string" } },
                        "dry_run": { "type": "boolean" }
                    },
                    "required": ["kind", "id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::MarketInstall,
            ),
            registration(
                "system.health",
                "Agent 健康状态",
                "读取本机 Agent 版本、能力网关、控制面和登录状态。stdio companion 下 dashboard_worker_state=not_applicable 属于正常状态；判断 Worker 是否异常请看 dashboard_worker_expected 和 dashboard_worker_state。",
                "read_only",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::SystemHealth,
            ),
            registration(
                "engineering.project.resolve",
                "解析工程开发上下文",
                "读取工作区 .himind/project.json，解析项目、目标展馆、环境和默认 Workflow，不修改工程文件。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "workspace_root": { "type": "string", "minLength": 1 },
                        "target": { "type": "string" },
                        "environment": { "type": "string" }
                    },
                    "required": ["workspace_root"],
                    "additionalProperties": false
                }),
                CapabilityHandler::EngineeringProjectResolve,
            ),
            registration(
                "engineering.checkpoint.create",
                "创建开发检查点",
                "读取真实 Git HEAD、分支、工作区状态和可选测试结果，生成不可变 Development Checkpoint；不修改业务源码。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "workspace_root": { "type": "string", "minLength": 1 },
                        "project_id": { "type": "string", "minLength": 1 },
                        "target_id": { "type": "string" },
                        "environment": { "type": "string" },
                        "tests": {
                            "type": "array",
                            "items": { "type": "string", "minLength": 1 },
                            "uniqueItems": true
                        },
                        "created_by": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "client": { "type": "string" },
                                "session_id": { "type": "string" },
                                "lease_id": { "type": "string" }
                            }
                        }
                    },
                    "required": ["workspace_root", "project_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::EngineeringCheckpointCreate,
            ),
            registration(
                "engineering.workspace.lease.acquire",
                "获取工作区租约",
                "为 DSH、外部 AI 或 Workflow 获取工作区读写租约，避免同一工程被并发修改。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "workspace_root": { "type": "string", "minLength": 1 },
                        "project_id": { "type": "string" },
                        "target_id": { "type": "string" },
                        "mode": { "enum": ["read", "write"] },
                        "owner_client": { "type": "string", "minLength": 1 },
                        "owner_session": { "type": "string" },
                        "ttl_seconds": { "type": "integer", "minimum": 60, "maximum": 86400 }
                    },
                    "required": ["workspace_root", "owner_client"],
                    "additionalProperties": false
                }),
                CapabilityHandler::EngineeringWorkspaceLeaseAcquire,
            ),
            registration(
                "engineering.workspace.lease.release",
                "释放工作区租约",
                "释放指定工作区租约。未知或已经释放的租约按幂等成功处理。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "lease_id": { "type": "string", "minLength": 1 }
                    },
                    "required": ["lease_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::EngineeringWorkspaceLeaseRelease,
            ),
            registration(
                "engineering.workspace.lease.list",
                "工作区租约列表",
                "列出当前有效的 DSH、外部 AI 和 Workflow 工作区租约。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "workspace_root": { "type": "string" }
                    },
                    "additionalProperties": false
                }),
                CapabilityHandler::EngineeringWorkspaceLeaseList,
            ),
            registration(
                "engineering.handoff.create",
                "创建 Workflow 交接",
                "把项目、DevelopmentCheckpoint、目标和 Workflow 选择固化为可审计 Handoff，供交付 Run 使用。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "project_id": { "type": "string", "minLength": 1 },
                        "target_id": { "type": "string" },
                        "environment": { "type": "string" },
                        "workspace_root": { "type": "string" },
                        "from_run_id": { "type": "string" },
                        "workflow_id": { "type": "string", "minLength": 1 },
                        "entrypoint": { "type": "string" },
                        "exitpoint": { "type": "string" },
                        "development_checkpoint": { "type": "object" },
                        "candidate": { "type": "object" },
                        "seed_artifacts": {
                            "type": "array",
                            "items": { "type": "string", "minLength": 1 },
                            "uniqueItems": true
                        },
                        "next_actions": {
                            "type": "array",
                            "items": { "type": "string", "minLength": 1 },
                            "uniqueItems": true
                        },
                        "notes": { "type": "string" }
                    },
                    "required": ["project_id", "workflow_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::EngineeringHandoffCreate,
            ),
            registration(
                "workflow.catalog.list",
                "Workflow 目录",
                "列出本机已安装 Workflow，包含生命周期、入口、出口和可用状态。",
                "read_only",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::WorkflowCatalogList,
            ),
            registration(
                "workflow.catalog.describe",
                "Workflow 详情",
                "按稳定 Workflow ID 返回版本、步骤、入口、出口、Artifact 和依赖摘要。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "workflow_id": { "type": "string", "minLength": 1 }
                    },
                    "required": ["workflow_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::WorkflowCatalogDescribe,
            ),
            registration(
                "workflow.run.start",
                "启动 Workflow Run",
                "校验 Preflight 并创建 Workflow Run；执行在后台继续，返回 Run 快照供 DSH 或外部 AI 跟踪。",
                "local_action",
                json!({
                    "type": "object",
                    "properties": {
                        "workflow_id": { "type": "string", "minLength": 1 },
                        "input": { "type": "object" },
                        "seed_checkpoint": { "type": "object" },
                        "handoff": { "type": "object" },
                        "execution": {
                            "type": "object",
                            "properties": {
                                "entrypoint": { "type": "string" },
                                "exitpoint": { "type": "string" },
                                "seed_artifacts": {
                                    "type": "array",
                                    "items": { "type": "string", "minLength": 1 },
                                    "uniqueItems": true
                                }
                            },
                            "additionalProperties": false
                        }
                    },
                    "required": ["workflow_id", "input"],
                    "additionalProperties": false
                }),
                CapabilityHandler::WorkflowRunStart,
            ),
            registration(
                "schedule.list",
                "定时任务列表",
                "列出本机定时任务（5 字段 cron、本地时区），并补齐下一次触发时间。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
                CapabilityHandler::ScheduleList,
            ),
            registration(
                "schedule.set",
                "设置定时任务",
                "创建或更新一条定时任务；目前支持 workflow 目标，到点由 Agent 调度器启动同一条 Run 路径。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "minLength": 1, "maxLength": 64 },
                        "kind": { "type": "string", "minLength": 1 },
                        "target_id": { "type": "string", "minLength": 1 },
                        "cron": { "type": "string", "minLength": 1 },
                        "input": { "type": "object" },
                        "execution": {
                            "type": "object",
                            "properties": {
                                "entrypoint": { "type": "string" },
                                "exitpoint": { "type": "string" }
                            },
                            "additionalProperties": false
                        },
                        "enabled": { "type": "boolean" }
                    },
                    "required": ["target_id", "cron"],
                    "additionalProperties": false
                }),
                CapabilityHandler::ScheduleSet,
            ),
            registration(
                "schedule.delete",
                "删除定时任务",
                "删除一条定时任务；已经启动的运行不受影响。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "minLength": 1, "maxLength": 64 }
                    },
                    "required": ["id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::ScheduleDelete,
            ),
            registration(
                "workflow.preset.list",
                "工作流启动预设",
                "列出已保存的启动参数预设；同一个工作流针对多个工作区复用时只改工作区。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "workflow_id": { "type": "string" }
                    },
                    "additionalProperties": false
                }),
                CapabilityHandler::WorkflowPresetList,
            ),
            registration(
                "workflow.preset.set",
                "保存工作流启动预设",
                "保存/更新一套启动参数（输入、入口出口），供后续一键启动复用。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "minLength": 1, "maxLength": 64 },
                        "workflow_id": { "type": "string", "minLength": 1 },
                        "label": { "type": "string" },
                        "input": { "type": "object" },
                        "entrypoint": { "type": "string" },
                        "exitpoint": { "type": "string" }
                    },
                    "required": ["workflow_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::WorkflowPresetSet,
            ),
            registration(
                "workflow.preset.delete",
                "删除工作流启动预设",
                "删除一条启动参数预设；不影响已经启动的运行。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "minLength": 1, "maxLength": 64 }
                    },
                    "required": ["id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::WorkflowPresetDelete,
            ),
            registration(
                "skill.run",
                "运行技能",
                "按技能说明（SKILL.md）与任务输入跑一次一次性 AI 运行，结果写入 Agent 的 skill-runs 记录。",
                "local_action",
                json!({
                    "type": "object",
                    "properties": {
                        "skill_id": { "type": "string", "minLength": 1 },
                        "task": { "type": "string", "minLength": 1 },
                        "workspace_root": { "type": "string" },
                        "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 86400 },
                        "tools": { "enum": ["none", "default"] },
                        "input": { "type": "object" }
                    },
                    "required": ["skill_id", "task"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SkillRunStart,
            ),
            registration(
                "skill.run.list",
                "技能运行记录",
                "列出最近的技能运行及其结果文件位置。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "limit": { "type": "integer", "minimum": 1, "maximum": 200 }
                    },
                    "additionalProperties": false
                }),
                CapabilityHandler::SkillRunList,
            ),
            registration(
                "skill.run.get",
                "读取技能运行",
                "读取一次技能运行的状态、结果预览与结果文件位置。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "run_id": { "type": "string", "minLength": 1 }
                    },
                    "required": ["run_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SkillRunGet,
            ),
            registration(
                "workflow.run.get",
                "读取 Workflow Run",
                "读取 Workflow Run、步骤、审批、Artifact、Events 和 Projection 状态。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "run_id": { "type": "string", "minLength": 1 }
                    },
                    "required": ["run_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::WorkflowRunGet,
            ),
            registration(
                "workflow.run.feedback",
                "提交 Workflow 反馈",
                "向等待反馈的 Development Loop 提交用户反馈，并在后台继续执行。",
                "local_action",
                json!({
                    "type": "object",
                    "properties": {
                        "run_id": { "type": "string", "minLength": 1 },
                        "feedback": { "type": "string", "minLength": 1 }
                    },
                    "required": ["run_id", "feedback"],
                    "additionalProperties": false
                }),
                CapabilityHandler::WorkflowRunFeedback,
            ),
            registration(
                "workflow.run.cancel",
                "取消 Workflow Run",
                "取消仍在执行的 Workflow Run，并同步中断关联审批。",
                "local_action",
                json!({
                    "type": "object",
                    "properties": {
                        "run_id": { "type": "string", "minLength": 1 },
                        "reason": { "type": "string" }
                    },
                    "required": ["run_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::WorkflowRunCancel,
            ),
            registration(
                "workflow.candidate.freeze",
                "冻结交付候选",
                "读取真实 Git HEAD、Tree Digest 和工作区状态，生成不可变 Candidate Artifact。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "project_root": { "type": "string" },
                        "repository_root": { "type": "string" },
                        "workspace_root": { "type": "string" },
                        "source_root": { "type": "string" },
                        "candidate_artifact_id": { "type": "string", "minLength": 1 },
                        "allow_dirty": { "type": "boolean" },
                        "development_checkpoint": { "type": "object" },
                        "workflow_context": { "type": "object" }
                    },
                    "required": ["candidate_artifact_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::WorkflowCandidateFreeze,
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
                "scan.projects",
                "扫描本机项目",
                "按本机扫描根目录和可选项目目标识别 Unity、Unreal 或通用工程候选。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": {
                        "source_roots": { "type": "array", "items": { "type": "string" } },
                        "release_roots": { "type": "array", "items": { "type": "string" } },
                        "scan_targets": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "project_name": { "type": "string" },
                                    "exhibit_name": { "type": "string" }
                                },
                                "additionalProperties": false
                            }
                        }
                    },
                    "additionalProperties": false
                }),
                CapabilityHandler::ScanProjects,
            ),
            registration(
                "inner_admin.sync_exhibits",
                "同步内网展项",
                "登录内网管理系统并同步当前用户待上传展项及工程信息。",
                "network_write",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::InnerAdminSyncExhibits,
            ),
            registration(
                "upload.code",
                "上传代码包",
                "按工程目录生成代码归档并分片上传到内网管理系统。",
                "network_write",
                json!({
                    "type": "object",
                    "properties": {
                        "pid": { "type": "string" },
                        "exhibit_name": { "type": "string" },
                        "package_type": { "type": "string", "enum": ["source", "release"] },
                        "source_path": { "type": "string" },
                        "release_path": { "type": "string" }
                    },
                    "required": ["pid"],
                    "additionalProperties": false
                }),
                CapabilityHandler::UploadCode,
            ),
            registration(
                "upload.placeholder",
                "上传占位文件",
                "生成占位说明文件并上传到内网管理系统。",
                "network_write",
                json!({
                    "type": "object",
                    "properties": {
                        "pid": { "type": "string" },
                        "exhibit_name": { "type": "string" },
                        "file_name": { "type": "string" },
                        "content": { "type": "string", "minLength": 1 }
                    },
                    "required": ["pid", "content"],
                    "additionalProperties": false
                }),
                CapabilityHandler::UploadPlaceholder,
            ),
            registration(
                "storage.smb.upload",
                "上传到 SMB",
                "将本机文件安全复制到指定 SMB 目录，并提供可取消的文件级进度。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "target_dir": { "type": "string" },
                        "source_paths": { "type": "array", "items": { "type": "string" } },
                        "relative_paths": { "type": "array", "items": { "type": "string" } },
                        "conflict_policy": { "type": "string", "enum": ["replace", "skip"] },
                        "category": {}
                    },
                    "required": ["target_dir", "source_paths"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SmbUpload,
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
                "使用检测到的 Unity/Unreal 原生工具链构建工程；项目脚本仅在显式选择 script 时执行。",
                "local_action",
                json!({
                    "type": "object",
                    "properties": {
                        "target_path": { "type": "string" },
                        "engine_type": { "type": ["string", "null"] },
                        "engine_version": { "type": ["string", "null"] },
                        "provider": { "type": "string", "enum": ["auto", "native", "script"] },
                        "target_platform": { "type": "string", "enum": ["windows", "linux", "macos", "webgl", "android", "ios"] },
                        "architecture": { "type": "string", "enum": ["x64", "arm64"] },
                        "configuration": { "type": "string", "enum": ["development", "shipping", "test", "release"] },
                        "output_path": { "type": ["string", "null"] },
                        "clean": { "type": "boolean" },
                        "wait": { "type": "boolean", "description": "为 true 时等待构建结束再返回，工作流步骤用；交互式调用保持默认异步。" },
                        "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 7200 }
                    },
                    "required": ["target_path"],
                    "additionalProperties": false
                }),
                CapabilityHandler::WorkspaceBuild,
            ),
            registration(
                "exhibit.workspace.build.status",
                "读取工程构建状态",
                "读取本机工程构建任务的状态和最近日志。",
                "read_only",
                json!({
                    "type": "object",
                    "properties": { "job_id": { "type": "string" } },
                    "required": ["job_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::WorkspaceBuildStatus,
            ),
            registration(
                "exhibit.workspace.build.cancel",
                "取消工程构建",
                "停止本机工程构建任务及其子进程。",
                "local_action",
                json!({
                    "type": "object",
                    "properties": { "job_id": { "type": "string" } },
                    "required": ["job_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::WorkspaceBuildCancel,
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
                "由 Edge Worker 使用受管 SVN 管理账号在项目仓库的 trunk/exhibits 下创建固定展项 ID 目录。",
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
                "使用当前用户的个人 SVN 凭据，将受控 Unity 或 Unreal 模板写入已准备好的展项目录并应用忽略属性。",
                "network_write",
                json!({
                    "type": "object",
                    "properties": {
                        "project_id": { "type": "string" },
                        "exhibit_id": { "type": "string" },
                        "engine_type": { "type": "string", "enum": ["Unity3D", "Unreal Engine"] },
                        "template_id": { "type": "string", "enum": ["unity-uniart", "unreal-blank-4.27", "unreal-blank-5.3", "unreal-blank-5.4", "unreal-blank-5.5", "unreal-picoxr-5.3", "unreal-picoxr-5.5"] },
                        "svn_username": { "type": "string", "minLength": 1 },
                        "prerequisite_task_id": { "type": "string", "minLength": 1 }
                    },
                    "required": ["project_id", "exhibit_id", "engine_type", "template_id", "svn_username", "prerequisite_task_id"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SvnExhibitRepositoryInitialize,
            ),
            registration(
                "exhibit.repository.clone",
                "克隆展项 SVN 仓库",
                "由 Edge Worker 在同一 SVN 服务中将源展项仓库复制到目标展项目录。",
                "admin_action",
                json!({
                    "type": "object",
                    "properties": {
                        "project_id": { "type": "string" },
                        "exhibit_id": { "type": "string" },
                        "source_repository_url": { "type": "string", "format": "uri" }
                    },
                    "required": ["project_id", "exhibit_id", "source_repository_url"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SvnExhibitRepositoryClone,
            ),
            registration(
                "exhibit.repository.import_local",
                "导入本地展项工程",
                "使用当前用户的个人 SVN 凭据将本地工程迁移到目标展项 SVN 仓库，并保留可验证的忽略规则、属性和外部依赖。",
                "network_write",
                json!({
                    "type": "object",
                    "properties": {
                        "project_id": { "type": "string" },
                        "exhibit_id": { "type": "string" },
                        "source_path": { "type": "string" },
                        "force_migration": { "type": "boolean" },
                        "expected_source_fingerprint": { "type": "string" },
                        "ignore_policy": { "type": "object" }
                    },
                    "required": ["project_id", "exhibit_id", "source_path"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SvnExhibitRepositoryImportLocal,
            ),
            registration(
                "project.repository.acl.preview",
                "预览项目 SVN 权限",
                "读取项目受管 SVN 路径的当前权限，并生成待应用的差异计划。",
                "read_only",
                project_acl_preview_schema(),
                CapabilityHandler::SvnProjectAclPreview,
            ),
            registration(
                "project.repository.acl.apply",
                "应用项目 SVN 权限",
                "校验预览摘要后应用项目 SVN 权限计划。",
                "R3",
                project_acl_apply_schema(),
                CapabilityHandler::SvnProjectAclApply,
            ),
            registration(
                "project.repository.acl.reconcile",
                "收敛项目 SVN 权限",
                "按期望状态收敛项目受管 SVN 路径的访问权限。",
                "R3",
                project_acl_reconcile_schema(),
                CapabilityHandler::SvnProjectAclReconcile,
            ),
            registration(
                "project.repository.create",
                "创建项目 SVN 仓库",
                "由 Edge Worker 使用节点安全存储中的 SvnAdmin 管理凭据，按项目唯一 ID 创建物理仓库。",
                "admin_action",
                json!({
                    "type": "object",
                    "properties": {
                        "project_id": { "type": "string" },
                        "project_name": { "type": "string" },
                        "hook_endpoint": { "type": "string", "format": "uri" },
                        "repository_access": { "type": "string" }
                    },
                    "required": ["project_id", "hook_endpoint"],
                    "additionalProperties": false
                }),
                CapabilityHandler::SvnRepositoryCreate,
            ),
            registration(
                "project.repository.exhibits_access.ensure",
                "配置项目展项目录访问权限",
                "由 Edge Worker 使用受管 SvnAdmin 凭据开放仓库祖先节点只读遍历、默认隔离展项目录，并保留具体展项用户 ACL。",
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
                "校验并原样保存不可变 .hmskill 或 .zip 候选包，返回 Skill 身份和 SHA-256。",
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
                "extension.skill.candidate.confirm",
                "确认 Skill 候选",
                "复验候选包哈希后确认当前版本，后续提交审核只能引用该不可变候选。",
                "local_write",
                authoring_identity_schema(),
                CapabilityHandler::SkillCandidateConfirm,
            ),
            registration(
                "extension.test",
                "测试扩展候选",
                "按插件、Skill 或 Workflow 类型执行候选测试并返回结构化报告。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["plugin", "skill", "workflow"] },
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
                "extension.plugin.candidate.confirm",
                "确认插件候选",
                "复验候选包哈希后确认当前版本，后续提交审核只能引用该不可变候选。",
                "local_write",
                authoring_identity_schema(),
                CapabilityHandler::PluginCandidateConfirm,
            ),
            registration(
                "extension.workflow.candidate.save",
                "保存 Workflow 候选",
                "校验 Workflow 源码目录，生成不可变 .hmwf Candidate，并返回 Workflow 身份和 SHA-256。",
                "local_write",
                json!({
                    "type": "object",
                    "properties": {
                        "workspace_root": { "type": "string" },
                        "source_root": { "type": "string" }
                    },
                    "required": ["workspace_root"],
                    "additionalProperties": false
                }),
                CapabilityHandler::WorkflowCandidateSave,
            ),
            registration(
                "extension.workflow.candidate.test",
                "测试 Workflow 候选",
                "重新生成候选制品并比对 SHA-256，校验 Workflow Manifest、资产和 Step DAG。",
                "local_write",
                authoring_identity_schema(),
                CapabilityHandler::WorkflowCandidateTest,
            ),
            registration(
                "extension.workflow.candidate.confirm",
                "确认 Workflow 候选",
                "复验 Candidate 哈希后确认当前版本，后续提交审核只能引用该不可变 Candidate。",
                "local_write",
                authoring_identity_schema(),
                CapabilityHandler::WorkflowCandidateConfirm,
            ),
            registration(
                "extension.workflow.submission.submit",
                "提交 Workflow 审核",
                "将已确认的 Workflow Candidate、依赖锁和本地测试报告提交到组织审核。",
                "network_write",
                authoring_identity_schema(),
                CapabilityHandler::WorkflowSubmissionSubmit,
            ),
            registration(
                "extension.workflow.submission.status",
                "Workflow 提审状态",
                "读取当前绑定用户的 Workflow 提审状态和审核意见。",
                "read_only",
                json!({ "type": "object", "additionalProperties": false }),
                CapabilityHandler::WorkflowSubmissionStatus,
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
                        "product_type": {"type":"string", "enum":["desktop_agent","agent_plugin","organization_skill","desktop_app","runtime_component","knowledge_edge_node","edge_node"]},
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
        // the same gateway as built-in and plugin capabilities. A downstream
        // that fails to start only removes its own tools, unless the user
        // marked it as required, in which case the missing toolset is an error
        // instead of a silent gap.
        match self.downstream_mcp.list_capabilities() {
            Ok(downstream) => {
                // 标记为「必须可用」的下游连不上时，宁可直接失败，也不要让会话
                // 拿着一套残缺的工具继续跑。
                if let Some(failure) = downstream.blocking_failures.into_iter().next() {
                    return Err(failure.into());
                }
                for (descriptor, _) in downstream.capabilities {
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
            // 读不了个人 MCP 配置不算致命：这类工具本就可选，缺了不影响其它能力。
            Err(_) => {}
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
        self.invoke_internal(context, capability_id, input, None)
    }

    pub(crate) fn invoke_with_execution_context(
        &self,
        context: &InvocationContext,
        capability_id: &str,
        input: Value,
        execution: &mut CapabilityExecutionContext<'_>,
    ) -> Result<Value, Box<dyn Error>> {
        self.invoke_internal(context, capability_id, input, Some(execution))
    }

    fn invoke_internal(
        &self,
        context: &InvocationContext,
        capability_id: &str,
        input: Value,
        mut execution: Option<&mut CapabilityExecutionContext<'_>>,
    ) -> Result<Value, Box<dyn Error>> {
        if is_svn_admin_capability(capability_id) {
            return Err("集中 SVN 管理能力仅由 Edge Worker 执行".into());
        }
        if capability_id == "exhibit.repository.initialize_template"
            && !is_edge_prepared_template_invocation(context, &input)
        {
            return Err(
                "展项模板写入必须由 Dashboard 创建的 Edge 前置任务释放后执行；请通过展项仓库初始化任务发起".into(),
            );
        }
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
                "message": "此能力由 AI 工作台提供；请在设置中开启「AI 工作台」后重试"
            })
            .to_string()
            .into());
        }
        validate_capability_input_schema(&registration.descriptor.input_schema, &input)?;
        // The pid/display-number rule belongs to Dashboard business
        // capabilities only. A local plugin is allowed to define its own
        // `exhibit_id` argument with unrelated semantics.
        validate_exhibit_route_id_input(&registration.descriptor, &input)?;
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
                && should_sync_remote_approval(self.options.mode(), &registration.descriptor)
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
        if let Some(scope) = required_platform_scope(capability_id) {
            crate::api::oauth::platform_access_token(&self.options, scope)?;
        }
        let _invocation_metadata = (
            context.source.as_str(),
            context.principal.as_str(),
            context.session_id_hash.as_str(),
            context.request_id.as_str(),
        );
        let agent_core_recording = if context.record_agent_core_run
            && should_record_agent_core_run(capability_id)
        {
            match crate::agent_core_service::AgentCoreRunRecorder::open_default() {
                Ok(recorder) => {
                    let agent_id = crate::api::client::load_agent_state(&self.options.state_path)
                        .map(|state| state.agent_id)
                        .unwrap_or_else(|_| "local-agent".to_string());
                    match recorder.begin(&agent_id, context, capability_id) {
                        Ok(run) => Some((recorder, run)),
                        Err(error) => {
                            eprintln!("local run begin failed for {capability_id}: {error}");
                            None
                        }
                    }
                }
                Err(error) => {
                    eprintln!("local run ledger unavailable for {capability_id}: {error}");
                    None
                }
            }
        } else {
            None
        };
        let result = match registration.handler {
            CapabilityHandler::SystemHealth => Ok(self.health(context)),
            CapabilityHandler::EngineeringProjectResolve => {
                let workspace = input
                    .get("workspace_root")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or("engineering.project.resolve requires workspace_root")?;
                let target = input
                    .get("target")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let environment = input
                    .get("environment")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let (project, workspace_root) = crate::engineering_project::load_from_workspace(
                    std::path::Path::new(workspace),
                )?;
                project.resolved_snapshot(&workspace_root, target, environment)
            }
            CapabilityHandler::EngineeringCheckpointCreate => {
                crate::development_checkpoint::create(&input)
            }
            CapabilityHandler::EngineeringWorkspaceLeaseAcquire => {
                crate::workspace_lease::acquire(&input)
            }
            CapabilityHandler::EngineeringWorkspaceLeaseRelease => {
                crate::workspace_lease::release(&input)
            }
            CapabilityHandler::EngineeringWorkspaceLeaseList => {
                crate::workspace_lease::list(&input)
            }
            CapabilityHandler::EngineeringHandoffCreate => crate::workflow_handoff::create(&input),
            CapabilityHandler::WorkflowCatalogList => {
                let store = crate::workflow::WorkflowStore::open_default()?;
                let items = store
                    .list()?
                    .into_iter()
                    .map(|item| {
                        json!({
                            "id": item.package.id,
                            "name": item.package.name,
                            "version": item.package.version,
                            "description": item.package.description,
                            "enabled": item.enabled,
                            "execution_policy": item.package.execution_policy,
                            "entrypoints": item.package.entrypoints,
                            "exits": item.package.exits,
                            "ui": item.package.ui,
                            "supported_runtimes": item.package.supported_runtimes,
                        })
                    })
                    .collect::<Vec<_>>();
                Ok(json!({ "ok": true, "workflows": items }))
            }
            CapabilityHandler::WorkflowCatalogDescribe => {
                let workflow_id = input
                    .get("workflow_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or("workflow.catalog.describe requires workflow_id")?;
                let item = crate::workflow::WorkflowStore::open_default()?
                    .list()?
                    .into_iter()
                    .find(|item| item.package.id == workflow_id)
                    .ok_or_else(|| format!("workflow not found: {workflow_id}"))?;
                Ok(json!({
                    "ok": true,
                    "enabled": item.enabled,
                    "previous_version": item.previous_version,
                    "package": item.package,
                }))
            }
            CapabilityHandler::WorkflowRunStart => {
                let workflow_id = input
                    .get("workflow_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or("workflow.run.start requires workflow_id")?;
                let mut workflow_input = input
                    .get("input")
                    .cloned()
                    .filter(Value::is_object)
                    .ok_or("workflow.run.start requires input object")?;
                if let Some(execution) = input.get("execution") {
                    if let Some(object) = workflow_input.as_object_mut() {
                        object.insert("execution".to_string(), execution.clone());
                    }
                }
                if let Some(checkpoint) = input.get("seed_checkpoint") {
                    if let Some(object) = workflow_input.as_object_mut() {
                        object.insert("development_checkpoint".to_string(), checkpoint.clone());
                    }
                }
                if let Some(handoff) = input.get("handoff") {
                    if let Some(object) = workflow_input.as_object_mut() {
                        object.insert("handoff".to_string(), handoff.clone());
                        if let Some(checkpoint) = handoff.get("development_checkpoint") {
                            object.insert("development_checkpoint".to_string(), checkpoint.clone());
                        }
                    }
                }
                crate::app::commands::schedule_workflow_with_gateway(
                    self.clone(),
                    workflow_id,
                    workflow_input,
                    context.clone(),
                )
            }
            CapabilityHandler::ScheduleList => {
                crate::scheduler::list(crate::scheduler::now_epoch())
            }
            CapabilityHandler::ScheduleSet => {
                crate::scheduler::set(&input, crate::scheduler::now_epoch())
            }
            CapabilityHandler::ScheduleDelete => {
                let id = input
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or("schedule.delete requires id")?;
                crate::scheduler::delete(id)
            }
            CapabilityHandler::WorkflowPresetList => {
                let workflow_id = input
                    .get("workflow_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                crate::workflow::list_run_presets(workflow_id)
            }
            CapabilityHandler::WorkflowPresetSet => {
                crate::workflow::set_run_preset(&input, crate::scheduler::now_epoch())
            }
            CapabilityHandler::WorkflowPresetDelete => {
                let id = input
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or("workflow.preset.delete requires id")?;
                crate::workflow::delete_run_preset(id)
            }
            CapabilityHandler::SkillRunStart => {
                let skill_id = input
                    .get("skill_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or("skill.run requires skill_id")?;
                let mut run_input = input
                    .get("input")
                    .cloned()
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({}));
                if let Some(object) = run_input.as_object_mut() {
                    for key in ["task", "workspace_root", "timeout_seconds"] {
                        if let Some(value) = input.get(key) {
                            object.insert(key.to_string(), value.clone());
                        }
                    }
                }
                crate::skill_run::start(self.options(), "", skill_id, &run_input)
            }
            CapabilityHandler::SkillRunList => {
                let limit = input
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map(|value| value as usize)
                    .unwrap_or(20);
                crate::skill_run::list(limit)
            }
            CapabilityHandler::SkillRunGet => {
                let run_id = input
                    .get("run_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or("skill.run.get requires run_id")?;
                let record = crate::skill_run::get(run_id)?
                    .ok_or_else(|| format!("技能运行不存在：{run_id}"))?;
                Ok(serde_json::to_value(record)?)
            }
            CapabilityHandler::WorkflowRunGet => {
                let run_id = input
                    .get("run_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or("workflow.run.get requires run_id")?;
                crate::app::commands::workflow_run_snapshot(run_id)
            }
            CapabilityHandler::WorkflowRunFeedback => {
                let run_id = input
                    .get("run_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or("workflow.run.feedback requires run_id")?;
                let feedback = input
                    .get("feedback")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or("workflow.run.feedback requires feedback")?;
                crate::app::commands::schedule_resume_workflow_with_gateway(
                    self.clone(),
                    run_id,
                    Some(feedback),
                )
            }
            CapabilityHandler::WorkflowRunCancel => {
                let run_id = input
                    .get("run_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or("workflow.run.cancel requires run_id")?;
                let reason = input
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or("workflow canceled through MCP");
                let runner = crate::workflow::WorkflowRunner::open_default()?;
                let ledger = crate::store::local_runs::LocalRunLedger::open_default()?;
                let run = ledger
                    .get_run(run_id)?
                    .ok_or_else(|| format!("workflow run not found: {run_id}"))?;
                let approval_id = if run.current_step_id.trim().is_empty() {
                    String::new()
                } else {
                    crate::workflow::workflow_approval_id(&run.run_id, &run.current_step_id)
                };
                let run = runner.cancel(run, reason)?;
                if !approval_id.is_empty() {
                    let _ = self
                        .approval_manager
                        .interrupt(&approval_id, "workflow_canceled");
                }
                Ok(serde_json::to_value(run)?)
            }
            CapabilityHandler::WorkflowCandidateFreeze => crate::workflow::freeze_candidate(&input),
            CapabilityHandler::CapabilityCatalogSearch => {
                Ok(self.search_capabilities(context, &input)?)
            }
            CapabilityHandler::CapabilityCatalogDescribe => {
                Ok(self.describe_capability(context, &input)?)
            }
            CapabilityHandler::MarketSearch => {
                let agent_id = self.paired_agent_id();
                Ok(crate::app::market::search(
                    &self.options,
                    &agent_id,
                    &input,
                )?)
            }
            CapabilityHandler::MarketInstalled => Ok(crate::app::market::installed(&input)?),
            CapabilityHandler::MarketInstallPlan => {
                let agent_id = self.paired_agent_id();
                Ok(crate::app::market::plan(&self.options, &agent_id, &input)?)
            }
            CapabilityHandler::MarketInstall => {
                let agent_id = self.paired_agent_id();
                Ok(crate::app::market::install(
                    &self.options,
                    &agent_id,
                    &input,
                    context.source,
                )?)
            }
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
                crate::extension_projects::current_workspace(input.get("workspace_root"))
            }
            CapabilityHandler::ExtensionWorkspaceBind => self.bind_extension_workspace(input),
            CapabilityHandler::ExtensionWorkspaceClear => self.clear_extension_workspace(input),
            CapabilityHandler::ExtensionRevisionCreate => self.create_extension_revision(input),
            CapabilityHandler::ExtensionLock => {
                Ok(serde_json::to_value(crate::app::extension_lock::load()?)?)
            }
            CapabilityHandler::ExtensionDistributionTargetGet => {
                self.read_distribution_targets(input)
            }
            CapabilityHandler::ExtensionDistributionTargetSet => {
                self.set_distribution_targets(input)
            }
            CapabilityHandler::ExtensionDistributionPreview => {
                let (kind, id, version) = distribution_identity(&input, true)?;
                crate::app::distribution_publish::preview(kind, &id, &version)
            }
            CapabilityHandler::ExtensionDistributionPublish => {
                let (kind, id, version) = distribution_identity(&input, true)?;
                let agent_id = self.load_paired_agent()?;
                let report = crate::app::distribution_publish::publish(
                    &self.options,
                    &agent_id,
                    kind,
                    &id,
                    &version,
                )?;
                Ok(report)
            }
            CapabilityHandler::ExtensionDistributionStateGet => {
                let view = crate::app::distribution_state::load()?;
                let kind = input
                    .get("kind")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                let id = input
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                let items = match (kind, id) {
                    (Some(kind), Some(id)) => view.for_asset(kind, id),
                    (None, None) => view.all(),
                    _ => return Err("kind 与 id 必须同时提供，或同时省略以读取全部记录".into()),
                };
                Ok(json!({ "items": items }))
            }
            CapabilityHandler::GithubAccountGet => Ok(serde_json::to_value(
                crate::store::github_credentials::status()?,
            )?),
            CapabilityHandler::GithubAccountSet => self.set_github_account(input),
            CapabilityHandler::GithubAccountRemove => {
                let removed = crate::store::github_credentials::remove()?;
                Ok(json!({
                    "state": "ready",
                    "removed": removed,
                    "account": crate::store::github_credentials::status()?,
                }))
            }
            CapabilityHandler::GithubAppAuthorizeStart => {
                let client_id = crate::app::github_app::configured_client_id();
                let authorization = crate::app::github_app::start_device_flow(&client_id)?;
                Ok(json!({
                    "state": "pending",
                    "client_id": client_id,
                    "authorization": authorization,
                    "next_steps": [
                        "在浏览器打开 verification_uri 并输入 user_code",
                        "按 interval 周期调用 github.app.authorize.poll，直到返回 authorized"
                    ]
                }))
            }
            CapabilityHandler::GithubAppAuthorizePoll => {
                let device_code = input
                    .get("device_code")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or("device_code is required")?;
                let client_id = crate::app::github_app::configured_client_id();
                match crate::app::github_app::poll_device_flow(&client_id, device_code)? {
                    crate::app::github_app::DevicePollOutcome::Authorized(token) => {
                        let record = crate::store::github_credentials::GithubAppRecord {
                            login: String::new(),
                            client_id: client_id.clone(),
                            installation_id: String::new(),
                            installation_account: String::new(),
                            user_token: token.access_token.clone(),
                            refresh_token: token.refresh_token.clone(),
                            user_token_expires_at: (crate::app::github_app::now_epoch()
                                + token.expires_in)
                                .to_string(),
                        };
                        crate::store::github_credentials::save_app_state(&record)?;
                        let installations =
                            crate::app::github_app::list_installations(&record.user_token)?;
                        Ok(json!({
                            "state": "authorized",
                            "installations": installations,
                            "next_steps": [
                                "调用 github.app.installation.select 绑定发布用的安装"
                            ]
                        }))
                    }
                    crate::app::github_app::DevicePollOutcome::Pending => {
                        Ok(json!({ "state": "pending" }))
                    }
                    crate::app::github_app::DevicePollOutcome::SlowDown => {
                        Ok(json!({ "state": "slow_down" }))
                    }
                    crate::app::github_app::DevicePollOutcome::Expired => {
                        Ok(json!({ "state": "expired" }))
                    }
                    crate::app::github_app::DevicePollOutcome::Denied => {
                        Ok(json!({ "state": "denied" }))
                    }
                }
            }
            CapabilityHandler::GithubAppInstallations => {
                let state = crate::store::github_credentials::app_state()?
                    .ok_or("GitHub App 尚未授权，请先调用 github.app.authorize.start")?;
                Ok(json!({
                    "installations": crate::app::github_app::list_installations(&state.user_token)?,
                }))
            }
            CapabilityHandler::GithubAppInstallationSelect => {
                let installation_id = input
                    .get("installation_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or("installation_id is required")?;
                let state = crate::store::github_credentials::app_state()?
                    .ok_or("GitHub App 尚未授权，请先调用 github.app.authorize.start")?;
                let selected = crate::app::github_app::list_installations(&state.user_token)?
                    .into_iter()
                    .find(|item| item.id == installation_id)
                    .ok_or("未找到该安装，请先调用 github.app.installations")?;
                crate::app::github_app::select_installation(&selected)?;
                Ok(json!({
                    "state": "ready",
                    "installation": selected,
                    "account": crate::store::github_credentials::status()?,
                }))
            }
            CapabilityHandler::ReleaseInstallPlan => {
                let (repository, tag, id, version) = release_install_identity(&input)?;
                Ok(serde_json::to_value(crate::app::release_install::plan(
                    &repository,
                    &tag,
                    &id,
                    &version,
                )?)?)
            }
            CapabilityHandler::ReleaseInstallApply => {
                let (repository, tag, id, version) = release_install_identity(&input)?;
                let dry_run = input
                    .get("dry_run")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let plan = crate::app::release_install::plan(&repository, &tag, &id, &version)?;
                let report = crate::app::release_install::install(&plan, dry_run)?;
                Ok(json!({ "plan": plan, "report": report }))
            }
            CapabilityHandler::InnerAdminLoginStatus => Ok(local_login_status_json()),
            CapabilityHandler::SystemOpenFolder => self.open_folder(input),
            CapabilityHandler::FilesystemDelete => self.filesystem_delete(input),
            CapabilityHandler::WorkspaceBuild => self.build_workspace(input),
            CapabilityHandler::WorkspaceBuildStatus => {
                let job_id = input
                    .get("job_id")
                    .and_then(Value::as_str)
                    .ok_or("job_id is required")?;
                workspace_build_status(job_id)
            }
            CapabilityHandler::WorkspaceBuildCancel => {
                let job_id = input
                    .get("job_id")
                    .and_then(Value::as_str)
                    .ok_or("job_id is required")?;
                cancel_workspace_build(job_id)
            }
            CapabilityHandler::WorkspaceStatus => self.workspace_status(input),
            CapabilityHandler::WorkspaceOpen => self.workspace_open(input),
            CapabilityHandler::RemoteConnect => self.remote_connect(input),
            CapabilityHandler::ScanProjects => crate::scan::service::execute_scan(Some(&input)),
            CapabilityHandler::InnerAdminSyncExhibits => {
                let mut fallback = CapabilityExecutionContext::detached(
                    input
                        .get("task_id")
                        .and_then(Value::as_str)
                        .unwrap_or("capability-sync"),
                    capability_id,
                    "inner_admin",
                );
                let execution = execution.as_deref_mut().unwrap_or(&mut fallback);
                crate::remote::sync::execute_sync_exhibits_with_context(&self.options, execution)
            }
            CapabilityHandler::UploadCode => {
                let mut fallback = CapabilityExecutionContext::detached(
                    input
                        .get("task_id")
                        .and_then(Value::as_str)
                        .unwrap_or("capability-upload"),
                    capability_id,
                    input
                        .get("source_path")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                );
                let execution = execution.as_deref_mut().unwrap_or(&mut fallback);
                crate::upload::tasks::execute_upload_code_with_context(
                    &self.options,
                    execution,
                    Some(&input),
                )
            }
            CapabilityHandler::UploadPlaceholder => {
                let mut fallback = CapabilityExecutionContext::detached(
                    input
                        .get("task_id")
                        .and_then(Value::as_str)
                        .unwrap_or("capability-placeholder"),
                    capability_id,
                    input.get("pid").and_then(Value::as_str).unwrap_or_default(),
                );
                let execution = execution.as_deref_mut().unwrap_or(&mut fallback);
                crate::upload::tasks::execute_upload_placeholder_with_context(
                    &self.options,
                    execution,
                    Some(&input),
                )
            }
            CapabilityHandler::SmbUpload => {
                let mut fallback = CapabilityExecutionContext::detached(
                    input
                        .get("task_id")
                        .and_then(Value::as_str)
                        .unwrap_or("capability-smb-upload"),
                    capability_id,
                    input
                        .get("target_dir")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                );
                let execution = execution.as_deref_mut().unwrap_or(&mut fallback);
                crate::upload::smb::execute_smb_upload_with_context(
                    &self.options,
                    execution,
                    Some(&input),
                )
            }
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
                let request = serde_json::from_value::<InitializeExhibitRepositoryRequest>(input)?;
                let mut fallback = CapabilityExecutionContext::detached(
                    execution
                        .as_deref()
                        .map(|value| value.task_id().to_string())
                        .unwrap_or_else(|| "capability-template-init".to_string()),
                    capability_id,
                    request.exhibit_id.clone(),
                );
                let execution = execution.as_deref_mut().unwrap_or(&mut fallback);
                let execution = std::cell::RefCell::new(execution);
                let mut check_cancelled = || execution.borrow_mut().check_cancelled();
                initialize_exhibit_repository_with_cancel(request, &mut check_cancelled)
            }
            CapabilityHandler::SvnExhibitRepositoryClone => {
                crate::svn::service::clone_exhibit_repository(serde_json::from_value::<
                    crate::svn::types::CloneExhibitRepositoryRequest,
                >(input)?)
            }
            CapabilityHandler::SvnExhibitRepositoryImportLocal => {
                let request =
                    serde_json::from_value::<crate::svn::types::ImportLocalExhibitRequest>(input)?;
                let mut fallback = CapabilityExecutionContext::detached(
                    execution
                        .as_deref()
                        .map(|value| value.task_id().to_string())
                        .unwrap_or_else(|| "capability-import-local".to_string()),
                    capability_id,
                    &request.source_path,
                );
                let execution = execution.as_deref_mut().unwrap_or(&mut fallback);
                let execution = std::cell::RefCell::new(execution);
                let mut check_cancelled = || execution.borrow_mut().check_cancelled();
                let mut report_progress = |progress: i32, detail: &str| {
                    execution.borrow_mut().report_progress(progress, detail)
                };
                crate::svn::service::import_local_exhibit_with_cancel_and_progress(
                    request,
                    &mut check_cancelled,
                    &mut report_progress,
                )
            }
            CapabilityHandler::SvnProjectExhibitsAccessEnsure => {
                ensure_project_exhibits_access(serde_json::from_value::<
                    EnsureProjectExhibitsAccessRequest,
                >(input)?)
            }
            CapabilityHandler::SvnProjectAclPreview => {
                crate::svn::service::preview_project_acl(serde_json::from_value::<
                    crate::svn::types::PreviewProjectAclRequest,
                >(input)?)
            }
            CapabilityHandler::SvnProjectAclApply => {
                crate::svn::service::apply_project_acl(serde_json::from_value::<
                    crate::svn::types::ApplyProjectAclRequest,
                >(input)?)
            }
            CapabilityHandler::SvnProjectAclReconcile => {
                crate::svn::service::reconcile_project_acl(serde_json::from_value::<
                    crate::svn::types::ReconcileProjectAclRequest,
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
            CapabilityHandler::SkillCandidateConfirm => {
                let (id, version) = authoring_identity(&input)?;
                Ok(serde_json::to_value(crate::skill::authoring::confirm(
                    &id, &version,
                )?)?)
            }
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
            CapabilityHandler::PluginCandidateConfirm => {
                let (id, version) = authoring_identity(&input)?;
                Ok(serde_json::to_value(crate::plugin_authoring::confirm(
                    &id, &version,
                )?)?)
            }
            CapabilityHandler::WorkflowCandidateSave => {
                validate_mcp_capability_workspace(context, capability_id, &input)?;
                let source_root = input
                    .get("source_root")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .or_else(|| {
                        input
                            .get("workspace_root")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                    })
                    .ok_or("workflow source_root or workspace_root is required")?;
                Ok(serde_json::to_value(
                    crate::workflow::save_authoring_candidate(Path::new(source_root))?,
                )?)
            }
            CapabilityHandler::WorkflowCandidateTest => {
                let (id, version) = authoring_identity(&input)?;
                let capabilities = self.list_capabilities(&InvocationContext::local_http())?;
                Ok(serde_json::to_value(
                    crate::workflow::test_authoring_candidate_with_capabilities(
                        &id,
                        &version,
                        &capabilities,
                    )?,
                )?)
            }
            CapabilityHandler::WorkflowCandidateConfirm => {
                let (id, version) = authoring_identity(&input)?;
                let capabilities = self.list_capabilities(&InvocationContext::local_http())?;
                Ok(serde_json::to_value(
                    crate::workflow::confirm_authoring_candidate_with_capabilities(
                        &id,
                        &version,
                        &capabilities,
                    )?,
                )?)
            }
            CapabilityHandler::WorkflowSubmissionSubmit => self.submit_workflow_candidate(input),
            CapabilityHandler::WorkflowSubmissionStatus => self.workflow_submission_status(),
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
                let output = invoke_plugin_capability(
                    &id,
                    input.clone(),
                    self.trusted_dashboard_url().as_deref(),
                )?;
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
        };
        if let Some((recorder, run)) = agent_core_recording {
            let recording_result = match &result {
                Ok(value) => recorder.complete(run, value),
                Err(error) => recorder.fail(run, &error.to_string()),
            };
            if let Err(error) = recording_result {
                eprintln!("local run completion failed for {capability_id}: {error}");
            }
        }
        result
    }

    pub(crate) fn health(&self, context: &InvocationContext) -> Value {
        let worker = local_worker_snapshot(&self.worker_status);
        let cached_capability_count = self.cached_capability_count();
        let mcp_stdio = context.transport == InvocationTransport::Stdio;
        let worker_state = if mcp_stdio {
            "not_applicable"
        } else {
            worker["dashboard_worker_state"]
                .as_str()
                .unwrap_or("unknown")
        };
        let worker_expected = if mcp_stdio {
            false
        } else {
            worker["dashboard_worker_expected"]
                .as_bool()
                .unwrap_or(false)
        };
        let mcp_transport = if mcp_stdio {
            InvocationTransport::Stdio.as_str()
        } else {
            worker["mcp_transport"]
                .as_str()
                .unwrap_or(InvocationTransport::LocalHttp.as_str())
        };
        let worker_error = if mcp_stdio || worker_state == "not_applicable" {
            Value::Null
        } else {
            worker["dashboard_worker_error"].clone()
        };
        let local_service_expected = !mcp_stdio;
        let worker_reason_code = if mcp_stdio {
            "stdio_companion_gateway_only"
        } else if worker_state == "not_applicable" && !self.options.mode().control_plane_enabled() {
            "independent_mode_no_control_plane"
        } else {
            worker["dashboard_worker_reason_code"]
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(match worker_state {
                    "online" => "connected_agent_app_worker",
                    "connecting" => "connected_agent_app_starting",
                    "offline" => "connected_agent_app_worker_error",
                    _ => "worker_status_unknown",
                })
        };
        let executable = local_agent_executable_metadata();
        json!({
            "status": "online",
            "runtime_schema_version": 1,
            "dashboard_worker_online_semantics": "legacy_boolean_use_expected_state",
            "version": VERSION,
            "mode": self.options.mode().as_str(),
            "dashboard_enabled": self.options.mode().dashboard_enabled(),
            "control_plane": json!({
                "kind": self.options.mode().control_plane(),
                "enabled": self.options.mode().control_plane_enabled(),
                "worker_online": worker["dashboard_worker_online"],
                "worker_state": worker_state,
                "worker_expected": worker_expected,
                "worker_reason_code": worker_reason_code,
                "available": self.options.mode().control_plane_enabled(),
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
            "dashboard_worker_error": worker_error,
            "dashboard_worker_state": worker_state,
            "dashboard_worker_expected": worker_expected,
            "dashboard_worker_reason_code": worker_reason_code,
            "mcp_transport": mcp_transport,
            "local_service_expected": local_service_expected,
            // The desktop Agent no longer owns the shared SvnAdmin account.
            // Keep explicit internal markers for older clients while making
            // the ownership boundary unambiguous.
            "svn_admin_ready": false,
            "svn_admin_status": "edge_worker_only",
            "local_service_online": worker["local_service_online"],
            "local_service_error": worker["local_service_error"],
            "capability_gateway": true,
            // Health must remain constant-time even when Dashboard, GitHub or
            // a downstream plugin is unavailable. The full catalog has its
            // own endpoint and is populated lazily.
            "capabilities": cached_capability_count.unwrap_or_default(),
            "capabilities_cached": cached_capability_count.is_some(),
            "local_port": self.options.local_port,
            "profile": crate::store::paths::profile_name(),
        })
    }

    /// Runtime facts used by MCP `initialize`. This is deliberately
    /// structured so clients can branch on mode/control-plane availability
    /// without making a second health call or parsing prose.
    pub(crate) fn mcp_runtime_metadata(&self) -> Value {
        json!({
            "schemaVersion": 1,
            "transport": InvocationTransport::Stdio.as_str(),
            "mode": self.options.mode().as_str(),
            "dashboardEnabled": self.options.mode().dashboard_enabled(),
            "dashboardWorkerState": "not_applicable",
            "dashboardWorkerExpected": false,
            "dashboardWorkerReasonCode": "stdio_companion_gateway_only",
            "localServiceExpected": false,
            "controlPlane": {
                "kind": self.options.mode().control_plane(),
                "enabled": self.options.mode().control_plane_enabled(),
                "available": self.options.mode().control_plane_enabled(),
            }
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
        let bound = Value::String(crate::extension_workspace::display_path(&path));
        let current = crate::extension_projects::current_workspace(Some(&bound))?;
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

    fn clear_extension_workspace(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        // 传入 workspace_root 时只解除该目录，避免一个会话清掉别的并发会话的工作区；
        // 不带参数时保留原有的"清除本机全部绑定"语义。
        let requested = input
            .get("workspace_root")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let removed =
            crate::extension_workspace::unbind(requested.map(Path::new)).map_err(|error| {
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
        let removed: Vec<String> = removed
            .iter()
            .map(|path| crate::extension_workspace::display_path(path))
            .collect();
        // 解除后回读的必须是"这次调用所属的会话"，否则并发会话会拿到别的会话目录。
        let current =
            crate::extension_projects::current_workspace(requested.map(Value::from).as_ref())?;
        Ok(json!({
            "state": "ready",
            "bound": false,
            "removed_workspace_roots": removed,
            "previous_workspace_root": removed.last(),
            "workspace": current,
            "next_steps": ["重新调用 extension.workspace.current 确认当前会话目录"]
        }))
    }

    fn read_distribution_targets(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let kind = input
            .get("kind")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let id = input
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        match (kind, id) {
            (Some(kind), Some(id)) => {
                let kind = crate::extension_projects::ExtensionProjectKind::parse(kind)?;
                let project = crate::extension_projects::get(&format!("{}:{}", kind.as_str(), id))?;
                Ok(distribution_target_payload(&project))
            }
            (None, None) => {
                let projects = crate::extension_projects::list()?
                    .into_iter()
                    .filter(|project| {
                        !project.distribution_targets.is_empty()
                            || !project.source_unit_key.trim().is_empty()
                    })
                    .map(|project| distribution_target_payload(&project))
                    .collect::<Vec<_>>();
                Ok(json!({
                    "projects": projects,
                    "available_targets": available_distribution_targets(),
                    "default_targets": ["workbench"]
                }))
            }
            _ => Err("kind 与 id 必须同时提供，或同时省略以读取全部项目".into()),
        }
    }

    fn set_distribution_targets(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let kind = input
            .get("kind")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or("kind is required")?;
        let id = input
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or("id is required")?;
        let kind = crate::extension_projects::ExtensionProjectKind::parse(kind)?;
        let inherit = input
            .get("inherit")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let targets = match input.get("targets") {
            Some(Value::Array(items)) => Some(
                items
                    .iter()
                    .map(|item| {
                        parse_distribution_target(item.as_str().ok_or("targets 只能包含字符串")?)
                    })
                    .collect::<Result<Vec<_>, Box<dyn Error>>>()?,
            ),
            Some(_) => return Err("targets 必须是数组".into()),
            None => None,
        };
        if inherit && targets.is_some() {
            return Err("inherit=true 与 targets 不能同时提供".into());
        }
        if !inherit && targets.is_none() {
            return Err("需要提供 targets，或传 inherit=true 以继承分发单元默认".into());
        }
        let project = crate::extension_projects::set_distribution_targets(
            kind,
            id,
            match (&targets, inherit) {
                (Some(targets), _) => Some(targets.as_slice()),
                (None, true) => None,
                (None, false) => unreachable!("targets 或 inherit 必居其一"),
            },
        )?;
        let payload = distribution_target_payload(&project);
        Ok(json!({
            "state": "ready",
            "kind": kind.as_str(),
            "id": id,
            "targets": payload["targets"].clone(),
            "source": payload["source"].clone(),
            "unit_targets": payload["unit_targets"].clone(),
            "available_targets": payload["available_targets"].clone(),
            "project": payload["project"].clone(),
            "next_steps": [
                "调用 extension.distribution.target.get 确认生效目标",
                "发布入口按目标集合决定投递工作台或 GitHub"
            ]
        }))
    }

    /// 校验并保存 GitHub 分发凭据。登录名以 GitHub 返回为准，不采信调用方输入。
    fn set_github_account(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let token = input
            .get("token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or("token is required")?;
        let token_kind = input
            .get("token_kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let repositories = match input.get("repositories") {
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| -> Box<dyn Error> {
                            "repositories 只能包含字符串".into()
                        })
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(_) => return Err("repositories 必须是数组".into()),
            None => Vec::new(),
        };
        let identity = crate::app::github_publisher::verify_token(token)?;
        let account = crate::store::github_credentials::set_account(
            &identity.login,
            token,
            token_kind,
            &repositories,
        )?;
        Ok(json!({
            "state": "ready",
            "account": account,
            "identity": { "login": identity.login, "id": identity.id },
            "next_steps": [
                "调用 extension.distribution.preview 确认发布计划",
                "调用 extension.distribution.publish 按目标发布"
            ]
        }))
    }

    fn authoring_preflight(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let kind = input
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if !matches!(kind.as_str(), "plugin" | "skill" | "workflow") {
            return Err(crate::extension_authoring::blocked_error(
                "unknown",
                vec![crate::extension_authoring::blocker(
                    "invalid_extension_kind",
                    "preflight",
                    "kind 必须是 plugin、skill 或 workflow",
                    "使用 kind=plugin、kind=skill 或 kind=workflow 重新调用",
                    false,
                )],
                Vec::new(),
                vec!["修正 kind 后重新调用 extension.authoring.preflight".to_string()],
            ));
        }

        let mut blockers = Vec::new();
        let requested_workspace = input
            .get("workspace_root")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        // 显式传入的 workspace_root 就是本次会话的工作区；只有调用方没传时，
        // 才回落到进程级会话环境变量、进程目录和历史绑定。
        let workspace_state = match crate::extension_workspace::resolve_root(requested_workspace) {
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
                    "extension_workspace_invalid",
                    "workspace",
                    error.to_string(),
                    "传入存在且可访问的 workspace_root，或重新调用 extension.workspace.current",
                    true,
                ));
                None
            }
        };
        let current = workspace_state.as_ref().map(|(path, _, _)| path.clone());
        if let Some((current, source, _bound)) = workspace_state.as_ref() {
            // 显式传入的 workspace_root（source = "request"）是会话自己声明的
            // 工作区，不构成"未绑定"。
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

        let required_tools = match kind.as_str() {
            "plugin" => vec![
                "extension.environment.preflight",
                "extension.plugin.scaffold",
                "extension.plugin.validate",
                "extension.plugin.build",
                "extension.plugin.package",
            ],
            "skill" => vec![
                "extension.environment.preflight",
                "extension.skill.scaffold",
                "extension.skill.validate",
                "extension.skill.package",
            ],
            _ => vec![
                "extension.environment.preflight",
                "extension.workflow.scaffold",
                "extension.workflow.validate",
                "extension.workflow.build",
                "extension.workflow.package",
                "extension.workflow.candidate.save",
                "extension.workflow.candidate.test",
                "extension.test",
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
                    "启用并重新安装扩展开发工具插件，然后重启 Agent",
                    true,
                ));
            }
        }
        match crate::capability::plugin::find_plugin("com.himind.extension-development-tools") {
            Ok(Some(plugin)) if plugin.enabled && plugin.error.is_none() => {
                let minimum = "1.4.0";
                if crate::skill::resolver::compare_versions(&plugin.version, minimum)
                    == std::cmp::Ordering::Less
                {
                    let hint = authoring_upgrade_hint(
                        "plugin",
                        "com.himind.extension-development-tools",
                        minimum,
                    );
                    blockers.push(crate::extension_authoring::blocker(
                        "extension_tools_plugin_outdated",
                        "toolchain",
                        format!(
                            "扩展开发工具版本 {} 低于最低要求 {}",
                            plugin.version, minimum
                        ),
                        format!("把扩展开发工具升级到 {minimum} 或更高后重新执行预检。{hint}"),
                        true,
                    ));
                }
            }
            Ok(Some(plugin)) => blockers.push(crate::extension_authoring::blocker(
                "extension_tools_plugin_unavailable",
                "toolchain",
                format!(
                    "扩展开发工具插件不可用{}",
                    plugin
                        .error
                        .as_deref()
                        .map(|value| format!(": {value}"))
                        .unwrap_or_default()
                ),
                "在本机启用扩展开发工具插件并重新执行预检",
                true,
            )),
            Ok(None) => blockers.push(crate::extension_authoring::blocker(
                "extension_tools_plugin_missing",
                "toolchain",
                "未安装扩展开发工具插件",
                "安装三件套中的扩展开发工具插件后重新执行预检",
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
            "plugin" => Some((
                "com.himind.skill.develop-himind-plugins",
                "1.7.0",
                "extension.plugin.scaffold",
            )),
            "skill" => Some((
                "com.himind.skill.develop-himind-skills",
                "1.8.0",
                "extension.skill.scaffold",
            )),
            _ => Some((
                "com.himind.skill.develop-himind-workflows",
                "1.0.0",
                "extension.workflow.scaffold",
            )),
        };
        if let Some((skill_id, minimum, required_capability)) = required_skill {
            match crate::skill::store::SkillStore::new().list_records() {
                Ok(records) => match records.iter().find(|record| record.manifest.id == skill_id) {
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
                    Some(record) => {
                        let hint = authoring_upgrade_hint("skill", skill_id, minimum);
                        blockers.push(crate::extension_authoring::blocker(
                            "authoring_skill_outdated",
                            "toolchain",
                            format!(
                                "{} 版本 {} 低于最低要求 {}",
                                record.manifest.name, record.manifest.version, minimum
                            ),
                            format!("把该三件套 Skill 升级到 {minimum} 或更高后重试。{hint}"),
                            true,
                        ));
                    }
                    None => blockers.push(crate::extension_authoring::blocker(
                        "authoring_skill_missing",
                        "toolchain",
                        format!("未安装 {skill_id}"),
                        "安装对应的插件开发助手、技能开发助手或工作流开发助手后重试",
                        true,
                    )),
                },
                Err(error) => blockers.push(crate::extension_authoring::blocker(
                    "authoring_skill_lookup_failed",
                    "toolchain",
                    format!("读取三件套 Skill 失败: {error}"),
                    "修复 Agent Skill Store 后重新执行预检",
                    true,
                )),
            }
        }

        // 扩展开发工具版本过低时，下游的「三件套缺少必需能力」和「Skill 契约未
        // 满足」都是同一个根因的派生症状：老插件不提供新契约，Skill 再有新版本也
        // 起不来。把它们指回根因，避免用户和模型照着字面去重装 Skill、重装能力，
        // 绕一圈却始终升不到达标版本。
        if blockers
            .iter()
            .any(|item| item.code == "extension_tools_plugin_outdated")
        {
            const ROOT_CAUSE: &str = "（根因是扩展开发工具版本过低，先按上一条把它升级到达标版本）";
            for item in blockers.iter_mut() {
                if matches!(
                    item.code.as_str(),
                    "extension_tool_missing" | "authoring_skill_contract_mismatch"
                ) {
                    item.remediation.push_str(ROOT_CAUSE);
                }
            }
        }

        let mut warnings = Vec::new();
        if !self.options.mode().dashboard_enabled() {
            warnings.push(crate::extension_authoring::warning(
                "本机可以完成本地创作、候选测试和客户端注册；提审与分发需要在对接 AI 工作台后执行。",
            ));
        } else if crate::api::client::load_agent_state(&self.options.state_path)
            .ok()
            .is_none_or(|state| state.agent_id.trim().is_empty())
        {
            warnings.push(crate::extension_authoring::warning(
                "已开启 AI 工作台对接，但 HiMind 账号尚未授权；本地创作不受影响，提审前需完成授权。",
            ));
        }
        let next_steps = if kind == "workflow" {
            vec![
                "调用 extension.workflow.scaffold 创建标准 Workflow 工程".to_string(),
                "调用 extension.workflow.validate、build、package 完成工程和制品检查".to_string(),
                "调用 extension.workflow.candidate.save 生成不可变 .hmwf Candidate".to_string(),
                "调用 extension.test 或 extension.workflow.candidate.test 完成 Candidate 验证"
                    .to_string(),
            ]
        } else {
            vec![
                "调用 extension.authoring.identity 获取作者资料".to_string(),
                format!("调用 extension.{kind}.scaffold 创建或更新工程"),
                format!("调用 extension.{kind}.validate、构建/打包能力生成候选制品"),
                "调用 extension.test 完成依赖、注册、运行时和清理闭环".to_string(),
            ]
        };
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
        if should_sync_remote_approval(self.options.mode(), descriptor) {
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
        let remote_approval_id = if approved
            && should_sync_remote_approval(self.options.mode(), descriptor)
        {
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
        let request: WorkspaceBuildRequest = serde_json::from_value(input)?;
        if request.target_path.trim().is_empty() {
            return Err("target_path is required".into());
        }
        launch_workspace_build(&request)
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
                "message": "此能力由 AI 工作台提供；请在设置中开启「AI 工作台」后重试"
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
            self.trusted_dashboard_url().as_deref(),
        )?;
        finalize_plugin_capability(context, capability_id, &params, output)
    }

    fn trusted_dashboard_url(&self) -> Option<String> {
        self.options
            .mode()
            .control_plane_enabled()
            .then(|| self.options.api_base())
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
            &self.options.api_base(),
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
            "workflow" => {
                let capabilities = self.list_capabilities(&InvocationContext::local_http())?;
                let result = crate::workflow::test_authoring_candidate_with_capabilities(
                    &id,
                    &version,
                    &capabilities,
                )
                .map_err(|error| authoring_operation_error("workflow", "test", error))?;
                Ok(json!({
                    "kind": "workflow",
                    "id": id,
                    "version": version,
                    "state": "passed",
                    "checks": result.test_report.get("checks").cloned().unwrap_or(Value::Null),
                    "result": result
                }))
            }
            _ => Err(crate::extension_authoring::blocked_error(
                "unknown",
                vec![crate::extension_authoring::blocker(
                    "invalid_extension_kind",
                    "test",
                    "kind must be plugin, skill or workflow",
                    "使用 kind=plugin、kind=skill 或 kind=workflow 重新调用 extension.test",
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
            &client, &self.options.api_base(), &agent_id, &access.token
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

    fn submit_workflow_candidate(&self, input: Value) -> Result<Value, Box<dyn Error>> {
        let (id, version) = authoring_identity(&input)?;
        let agent_id = self.load_paired_agent()?;
        Ok(serde_json::to_value(
            crate::workflow::submit_authoring_candidate(&self.options, &agent_id, &id, &version)?,
        )?)
    }

    fn workflow_submission_status(&self) -> Result<Value, Box<dyn Error>> {
        let agent_id = self.load_paired_agent()?;
        let access = crate::api::oauth::platform_access_token(
            &self.options,
            crate::api::oauth::CREATIVE_SUBMIT_SCOPE,
        )?;
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?;
        crate::api::distribution::workflow_submissions(
            &client,
            &self.options.api_base(),
            &agent_id,
            &access.token,
        )
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
            &client, &self.options.api_base(), &agent_id, &access.token
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
            &self.options.api_base(),
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
            &self.options.api_base(),
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
            &self.options.api_base(),
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
            return Err("HiMind 账号尚未授权".into());
        }
        self.options.set_agent_credential(&state.credential);
        Ok(state.agent_id)
    }

    /// 市场能力的授权要求比工作台能力更宽松：本地与 GitHub 扩展源不依赖工作台，
    /// 所以未授权时也要能搜索和安装它们。工作台来源所需的授权由市场侧在计划里
    /// 明确记为 blocked_reasons，而不是在这里一刀切地把整个市场关掉。
    fn paired_agent_id(&self) -> String {
        match crate::api::client::load_agent_state(&self.options.state_path) {
            Ok(state) => {
                self.options.set_agent_credential(&state.credential);
                state.agent_id
            }
            Err(_) => String::new(),
        }
    }
}

fn capability_catalog_generation(capabilities: &[CapabilityDescriptor]) -> String {
    let mut hasher = Sha256::new();
    for capability in capabilities {
        for field in [
            capability.id.as_bytes(),
            capability.version.as_bytes(),
            capability.name.as_bytes(),
            capability.description.as_bytes(),
            capability.source.as_bytes(),
            capability.contract_source.as_bytes(),
            capability
                .contract_generation
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
        ] {
            hasher.update((field.len() as u64).to_le_bytes());
            hasher.update(field);
        }
        if let Ok(schema) = serde_json::to_vec(&capability.input_schema) {
            hasher.update((schema.len() as u64).to_le_bytes());
            hasher.update(schema);
        }
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn capability_catalog_filter_generation(
    query: &str,
    group: &str,
    surface: &str,
    availability: &str,
    source: &str,
) -> String {
    let mut hasher = Sha256::new();
    for field in [query, group, surface, availability, source] {
        hasher.update((field.len() as u64).to_le_bytes());
        hasher.update(field.as_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn format_capability_catalog_cursor(
    generation: &str,
    filter_generation: &str,
    offset: usize,
) -> String {
    format!("generation:{generation}|filter:{filter_generation}|offset:{offset}")
}

fn parse_capability_catalog_cursor(
    cursor: Option<&str>,
    generation: &str,
    filter_generation: &str,
) -> Result<usize, Box<dyn Error>> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let Some(cursor) = cursor.strip_prefix("generation:") else {
        return Err("invalid capability catalog cursor".into());
    };
    let Some((cursor_generation, cursor)) = cursor.split_once("|filter:") else {
        return Err("invalid capability catalog cursor".into());
    };
    let Some((cursor_filter, offset)) = cursor.split_once("|offset:") else {
        return Err("invalid capability catalog cursor".into());
    };
    if cursor_generation != generation || cursor_filter != filter_generation {
        return Err("capability catalog cursor is stale; request the first page again".into());
    }
    offset
        .parse::<usize>()
        .map_err(|_| "invalid capability catalog cursor".into())
}

fn is_svn_admin_capability(capability_id: &str) -> bool {
    matches!(
        capability_id,
        "svn.user.provision"
            | "project.repository.create"
            | "project.repository.exhibits_access.ensure"
            | "exhibit.repository_path.create"
            | "exhibit.repository.initialize"
            | "exhibit.repository.clone"
            | "project.repository.acl.preview"
            | "project.repository.acl.apply"
            | "project.repository.acl.reconcile"
            | "project.repository.archive"
            | "exhibit.repository.restore"
            | "project.repository.prune"
            | "project.repository.archive_drill"
    )
}

fn is_task_scoped_capability(capability_id: &str) -> bool {
    capability_id == "exhibit.repository.initialize_template"
}

/// Template writes use the signed-in user's SVN account, but the target path
/// and ACL are prepared centrally. Keep this capability task-scoped so an MCP,
/// Tauri or local HTTP caller cannot guess a repository ID and bypass Edge.
fn is_edge_prepared_template_invocation(context: &InvocationContext, input: &Value) -> bool {
    if context.source != crate::capability::types::InvocationSource::DashboardWorker {
        return false;
    }
    let task_type = context
        .business_context
        .get("task_type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if task_type != "exhibit_repository_initialize_template" {
        return false;
    }
    if context
        .business_context
        .get("source")
        .and_then(Value::as_str)
        .map(str::trim)
        != Some("dashboard")
    {
        return false;
    }
    let task_id = context
        .business_context
        .get("task_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let context_prerequisite_task_id = context
        .business_context
        .get("prerequisite_task_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let prerequisite_task_id = input
        .get("prerequisite_task_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let svn_username = input
        .get("svn_username")
        .and_then(Value::as_str)
        .unwrap_or_default();
    !task_id.trim().is_empty()
        && !prerequisite_task_id.trim().is_empty()
        && !svn_username.trim().is_empty()
        && prerequisite_task_id.trim() == context_prerequisite_task_id.trim()
}

fn project_acl_entry_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" },
            "username": { "type": "string" },
            "access": { "type": "string" }
        },
        "required": ["path", "username", "access"],
        "additionalProperties": false
    })
}

fn project_acl_preview_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "plan_id": { "type": "string" },
            "project_id": { "type": "string" },
            "managed_paths": { "type": ["array", "null"], "items": { "type": "string" } },
            "desired_entries": { "type": ["array", "null"], "items": project_acl_entry_schema() },
            "repository_access": { "type": "string" }
        },
        "required": ["plan_id", "project_id"],
        "additionalProperties": false
    })
}

fn project_acl_apply_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "plan_id": { "type": "string" },
            "project_id": { "type": "string" },
            "managed_paths": { "type": ["array", "null"], "items": { "type": "string" } },
            "desired_entries": { "type": ["array", "null"], "items": project_acl_entry_schema() },
            "expected_current_digest": { "type": "string" },
            "repository_access": { "type": "string" }
        },
        "required": ["plan_id", "project_id", "expected_current_digest"],
        "additionalProperties": false
    })
}

fn project_acl_reconcile_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "project_id": { "type": "string" },
            "managed_paths": { "type": ["array", "null"], "items": { "type": "string" } },
            "desired_entries": { "type": ["array", "null"], "items": project_acl_entry_schema() },
            "repository_access": { "type": "string" }
        },
        "required": ["project_id"],
        "additionalProperties": false
    })
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

fn should_record_agent_core_run(capability_id: &str) -> bool {
    !matches!(
        capability_id,
        "system.health"
            | "capability.catalog.search"
            | "capability.catalog.describe"
            | "capability.catalog.activate"
            | "capability.catalog.invoke"
    )
}

fn required_platform_scope(capability_id: &str) -> Option<&'static str> {
    match capability_id {
        "extension.skill.submission.submit"
        | "extension.skill.submission.status"
        | "extension.plugin.submission.submit"
        | "extension.plugin.submission.status"
        | "extension.workflow.submission.submit"
        | "extension.workflow.submission.status" => Some(crate::api::oauth::CREATIVE_SUBMIT_SCOPE),
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
        CapabilityHandler::WorkspaceBuild
            | CapabilityHandler::SvnWorkspaceCheckout
            | CapabilityHandler::InnerAdminSyncExhibits
            | CapabilityHandler::UploadCode
            | CapabilityHandler::UploadPlaceholder
            | CapabilityHandler::SmbUpload
            | CapabilityHandler::SvnExhibitRepositoryImportLocal
            | CapabilityHandler::DashboardExhibitWorkspaceCheckout
            | CapabilityHandler::SoftwareDistributionPublish
            | CapabilityHandler::MediaSubmit(_, _)
            | CapabilityHandler::MediaJobCancel
    );
    descriptor.execution_mode = if long_running { "long_running" } else { "sync" }.to_string();
    descriptor.supports_progress = long_running;
    descriptor.supports_cancel = matches!(
        handler,
        CapabilityHandler::WorkspaceBuild
            | CapabilityHandler::SvnWorkspaceCheckout
            | CapabilityHandler::InnerAdminSyncExhibits
            | CapabilityHandler::UploadCode
            | CapabilityHandler::UploadPlaceholder
            | CapabilityHandler::SmbUpload
            | CapabilityHandler::SvnExhibitRepositoryImportLocal
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
                    // 从市场装能力会写入本机磁盘，属于"改变我这台机器"的操作，
                    // 必须有人确认，不能由模型单方面批准。
                    | CapabilityHandler::MarketInstall
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
            | "extension.workflow.scaffold"
            | "extension.workflow.validate"
            | "extension.workflow.build"
            | "extension.workflow.package"
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
            | CapabilityHandler::WorkflowSubmissionSubmit
            | CapabilityHandler::WorkflowSubmissionStatus
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

/// Dashboard approval facts belong to organization control-plane operations.
/// Local plugins and downstream MCP capabilities may still require the Agent's
/// local approval policy, but must never turn a Connected local workflow into
/// a Dashboard OAuth dependency.
fn should_sync_remote_approval(
    mode: crate::app::runtime_mode::AgentMode,
    descriptor: &CapabilityDescriptor,
) -> bool {
    crate::approval::ownership::requires_dashboard_fact(
        mode.dashboard_enabled(),
        descriptor.dashboard_provider,
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
pub(crate) fn validate_capability_input_schema(
    schema: &Value,
    input: &Value,
) -> Result<(), Box<dyn Error>> {
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

fn validate_exhibit_route_id_input(
    descriptor: &CapabilityDescriptor,
    input: &Value,
) -> Result<(), Box<dyn Error>> {
    if !descriptor.dashboard_provider {
        return Ok(());
    }
    let Some(value) = input.get("exhibit_id").and_then(Value::as_str) else {
        return Ok(());
    };
    let normalized = value.trim();
    let is_display_number = normalized
        .get(..3)
        .map(|prefix| {
            prefix.eq_ignore_ascii_case("ex-")
                && normalized
                    .get(3..)
                    .map(|suffix| {
                        !suffix.is_empty()
                            && suffix.chars().all(|character| character.is_ascii_digit())
                    })
                    .unwrap_or(false)
        })
        .unwrap_or(false);
    if is_display_number {
        return Err(serde_json::json!({
            "code": "EXHIBIT_ROUTE_ID_REQUIRED",
            "field": "exhibit_id",
            "display_id": normalized,
            "message": format!("展项参数使用了展示编号 {normalized}。请先调用 business.exhibit.list 或 context.resolve，并使用返回项的 pid 作为 exhibit_id。"),
            "hint": "EX-xxxx 仅用于展示；后续展项读取、人员、需求和工作区操作必须传入 list 返回的 pid。"
        })
        .to_string()
        .into());
    }
    Ok(())
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

const EXHIBIT_ROUTE_ID_DESCRIPTION: &str =
    "使用 business.exhibit.list 或 context.resolve 返回的 pid（展项路由 ID）；exhibit_id（如 EX-0021）只是展示编号，不能直接使用。";

/// Add the stable exhibit identifier rule to every projected business
/// capability. Dashboard catalogs are allowed to update schemas at runtime,
/// so this normalization belongs at the final Gateway projection boundary
/// instead of being duplicated in each provider contract.
fn annotate_business_exhibit_id_contract(descriptor: &mut CapabilityDescriptor) {
    if descriptor.id == "business.exhibit.list" {
        let suffix =
            " 返回项中的 pid 才能作为后续 exhibit_id；exhibit_id（如 EX-0021）只是展示编号。";
        if !descriptor.description.contains("pid") {
            descriptor.description.push_str(suffix);
        }
    }
    let Some(properties) = descriptor
        .input_schema
        .get_mut("properties")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let Some(exhibit_id) = properties.get_mut("exhibit_id") else {
        return;
    };
    let Some(schema) = exhibit_id.as_object_mut() else {
        return;
    };
    schema.insert(
        "description".to_string(),
        Value::String(EXHIBIT_ROUTE_ID_DESCRIPTION.to_string()),
    );
}

fn available_distribution_targets() -> Vec<&'static str> {
    vec!["workbench", "github"]
}

/// 发布相关能力的公共入参解析：kind / id / version。
fn distribution_identity(
    input: &Value,
    require_version: bool,
) -> Result<
    (
        crate::extension_projects::ExtensionProjectKind,
        String,
        String,
    ),
    Box<dyn Error>,
> {
    let kind_value = input
        .get("kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("kind is required")?;
    let kind = crate::extension_projects::ExtensionProjectKind::parse(kind_value)?;
    let id = input
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("id is required")?
        .to_string();
    let version = input
        .get("version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    match (version, require_version) {
        (Some(version), _) => Ok((kind, id, version)),
        (None, false) => Ok((kind, id, String::new())),
        (None, true) => Err("version is required".into()),
    }
}

/// Release 安装相关能力的公共入参解析。
fn release_install_identity(
    input: &Value,
) -> Result<(String, String, String, String), Box<dyn Error>> {
    let read = |key: &str| -> Result<String, Box<dyn Error>> {
        input
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| -> Box<dyn Error> { format!("{key} is required").into() })
    };
    Ok((
        read("repository")?,
        read("tag")?,
        read("id")?,
        read("version")?,
    ))
}

fn parse_distribution_target(value: &str) -> Result<DistributionTarget, Box<dyn Error>> {
    DistributionTarget::parse(value).ok_or_else(|| -> Box<dyn Error> {
        format!("分发目标必须是 workbench 或 github，收到: {value}").into()
    })
}

fn distribution_target_payload(project: &crate::extension_projects::ExtensionProject) -> Value {
    let unit_targets = crate::extension_projects::unit_distribution_targets_for(
        project.kind,
        &project.extension_id,
    );
    json!({
        "kind": project.kind.as_str(),
        "id": project.extension_id,
        "project_id": project.id,
        "name": project.name,
        "unit_key": project.source_unit_key,
        "targets": project
            .distribution_targets
            .iter()
            .map(|target| target.as_str())
            .collect::<Vec<_>>(),
        "source": project.distribution_targets_source,
        "declared_targets": project
            .distribution_targets_declared
            .iter()
            .map(|target| target.as_str())
            .collect::<Vec<_>>(),
        "unit_targets": unit_targets
            .iter()
            .map(|target| target.as_str())
            .collect::<Vec<_>>(),
        "available_targets": available_distribution_targets(),
        "project": project,
    })
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
        | CapabilityHandler::ExtensionDistributionTargetGet
        | CapabilityHandler::ExtensionDistributionTargetSet
        | CapabilityHandler::ExtensionDistributionPreview
        | CapabilityHandler::ExtensionDistributionPublish
        | CapabilityHandler::ExtensionDistributionStateGet
        | CapabilityHandler::GithubAccountGet
        | CapabilityHandler::GithubAccountSet
        | CapabilityHandler::GithubAccountRemove
        | CapabilityHandler::GithubAppAuthorizeStart
        | CapabilityHandler::GithubAppAuthorizePoll
        | CapabilityHandler::GithubAppInstallations
        | CapabilityHandler::GithubAppInstallationSelect
        | CapabilityHandler::ReleaseInstallPlan
        | CapabilityHandler::ReleaseInstallApply
        | CapabilityHandler::ExtensionTest
        | CapabilityHandler::SkillCandidateSave
        | CapabilityHandler::SkillCandidateTest
        | CapabilityHandler::SkillCandidateConfirm
        | CapabilityHandler::SkillClientRegister
        | CapabilityHandler::SkillClientUnregister
        | CapabilityHandler::SkillClientsUnregister
        | CapabilityHandler::PluginCandidateSave
        | CapabilityHandler::PluginCandidateTest
        | CapabilityHandler::PluginCandidateConfirm
        | CapabilityHandler::WorkflowCandidateSave
        | CapabilityHandler::WorkflowCandidateTest
        | CapabilityHandler::WorkflowCandidateConfirm => CapabilityAvailability::Local,
        CapabilityHandler::MarketSearch
        | CapabilityHandler::MarketInstalled
        | CapabilityHandler::MarketInstallPlan
        | CapabilityHandler::MarketInstall => CapabilityAvailability::Local,
        CapabilityHandler::WorkflowSubmissionSubmit
        | CapabilityHandler::WorkflowSubmissionStatus
        | CapabilityHandler::PluginSubmissionSubmit
        | CapabilityHandler::PluginSubmissionStatus
        | CapabilityHandler::SkillSubmissionSubmit
        | CapabilityHandler::SkillSubmissionStatus => CapabilityAvailability::ControlPlane,
        CapabilityHandler::ExtensionReviewQueue
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
        | CapabilityHandler::SvnExhibitRepositoryClone
        | CapabilityHandler::SvnProjectExhibitsAccessEnsure
        | CapabilityHandler::SvnProjectAclPreview
        | CapabilityHandler::SvnProjectAclApply
        | CapabilityHandler::SvnProjectAclReconcile => CapabilityAvailability::ControlPlane,
        CapabilityHandler::SvnExhibitRepositoryInitialize => CapabilityAvailability::NetworkService,
        CapabilityHandler::InnerAdminSyncExhibits
        | CapabilityHandler::UploadCode
        | CapabilityHandler::UploadPlaceholder => CapabilityAvailability::NetworkService,
        CapabilityHandler::SmbUpload => CapabilityAvailability::Local,
        CapabilityHandler::SvnExhibitRepositoryImportLocal => CapabilityAvailability::ControlPlane,
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
        validate_extension_workspace_root(workspace_root)
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

    // 同一进程可能同时服务多个 HiMind AI 工作区会话，调用方传入的 workspace_root
    // 就是本次会话的工作区，不再要求它等于某个全局工作区。这里只挡住真实危险的
    // 情况：目录不存在，或者指向 Agent 自身安装目录 / 数据目录。
    crate::extension_workspace::validate_authoring_root(workspace_root).map_err(|message| {
        crate::extension_authoring::blocked_error(
            "unknown",
            vec![crate::extension_authoring::blocker(
                "extension_workspace_invalid",
                "workspace",
                message,
                "传入存在且可访问的扩展聚合仓库、插件或 Skill 目录",
                true,
            )],
            Vec::new(),
            vec!["修正 workspace_root 后重新调用扩展开发能力".to_string()],
        )
    })?;
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
                    "传入工作区内已生成并校验的 .hmpkg、.hmskill 或 .zip 文件",
                    false,
                )],
                Vec::new(),
                vec!["重新调用候选保存能力并传入 package_path".to_string()],
            )
        })?;
    let package = Path::new(package_path).canonicalize().map_err(|error| {
        crate::extension_authoring::blocked_error(
            "unknown",
            vec![crate::extension_authoring::blocker(
                "extension_package_invalid",
                "package",
                format!("无法访问候选包: {error}"),
                "传入存在且可访问的 .hmpkg、.hmskill 或 .zip 文件",
                true,
            )],
            Vec::new(),
            vec!["重新构建并打包扩展后重试".to_string()],
        )
    })?;
    if !package.is_file() {
        return Err(crate::extension_authoring::blocked_error(
            "unknown",
            vec![crate::extension_authoring::blocker(
                "extension_package_invalid",
                "package",
                "候选包必须是文件".to_string(),
                "传入存在且可访问的 .hmpkg、.hmskill 或 .zip 文件",
                false,
            )],
            Vec::new(),
            vec!["重新构建并打包扩展后重试".to_string()],
        ));
    }
    // 候选包必须来自开发者自己的工作区。工作区按次解析：显式传入的
    // workspace_root、当前会话环境变量、以及本 Agent 记住的工作区都算数，
    // 这样多个并发会话各自保存候选时不会互相冲突。
    let roots = crate::extension_workspace::known_authoring_roots(
        input.get("workspace_root").and_then(Value::as_str),
    );
    let inside_workspace = roots.iter().any(|root| package.starts_with(root));
    if roots.is_empty() || !inside_workspace {
        let hint = roots
            .first()
            .map(|root| crate::extension_workspace::display_path(root))
            .unwrap_or_default();
        return Err(crate::extension_authoring::blocked_error(
            "unknown",
            vec![crate::extension_authoring::blocker(
                "extension_workspace_unbound",
                "workspace",
                if hint.is_empty() {
                    "候选包必须位于已绑定或当前会话的扩展工作区内".to_string()
                } else {
                    format!("候选包必须位于扩展工作区内: {hint}")
                },
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

/// 扩展能力的工作区门禁。
///
/// 工作区由调用方按次传入 —— 同一个 Agent 进程会同时服务多个 HiMind AI 工作区
/// 会话，硬性比对某个全局工作区会让第二个会话直接不可用。这里只要求它指向一个
/// 真实存在、且不是 Agent 自身安装目录或数据目录的目录。
fn validate_extension_workspace_root(requested: &str) -> Result<(), Box<dyn Error>> {
    if let Err(message) = crate::extension_workspace::validate_authoring_root(requested) {
        return Err(serde_json::json!({
            "code": "extension_workspace_invalid",
            "message": message
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
            | "edge_node"
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

/// 工具链阻塞的升级提示：把「本机哪个扩展来源能拿到达标版本」一并说清。
///
/// 组织通道和本地/GitHub 来源的版本可能长期错位，只回报「版本太低」会让模型和
/// 用户都停在原地：既不知道本机能不能升，也不知道去哪升。这里复用扩展来源快照
/// （内存缓存 + 跨进程文件锁，成本可接受），供 preflight 的 remediation 直接给出
/// 出处；快照不可用时不追加任何说法，避免编造来源。
fn authoring_upgrade_hint(asset_kind: &str, asset_id: &str, minimum: &str) -> String {
    let candidates: Vec<(String, String)> = match asset_kind {
        "plugin" => crate::app::extension_source::plugin_versions(asset_id)
            .unwrap_or_default()
            .into_iter()
            .map(|item| (item.version, item.source))
            .collect(),
        "skill" => crate::app::extension_source::skill_versions(asset_id)
            .unwrap_or_default()
            .into_iter()
            .map(|item| (item.version, item.source))
            .collect(),
        _ => Vec::new(),
    };
    let Some(best) = candidates
        .iter()
        .max_by(|left, right| crate::skill::resolver::compare_versions(&left.0, &right.0))
    else {
        return format!(
            "本机扩展来源里没有 {asset_id} 的任何镜像，先在「我的能力 → 来源管理」添加包含它的来源。"
        );
    };
    if crate::skill::resolver::compare_versions(&best.0, minimum) != std::cmp::Ordering::Less {
        format!(
            "本机扩展来源最高可拿到 {}（来源 {}）：在「我的能力」里安装该来源版本即可。",
            best.0, best.1
        )
    } else {
        format!(
            "本机所有来源最高只有 {}（来源 {}），需要先把 ≥{} 的版本发布到组织/开发环境通道。",
            best.0, best.1, minimum
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::types::{InvocationSource, InvocationTransport};

    #[test]
    fn svn_admin_capabilities_are_worker_only() {
        assert!(is_svn_admin_capability("project.repository.create"));
        assert!(is_svn_admin_capability("svn.user.provision"));
        assert!(is_svn_admin_capability("project.repository.archive"));
        assert!(!is_svn_admin_capability("exhibit.workspace.checkout"));
        assert!(!is_svn_admin_capability("exhibit.repository.import_local"));
        assert!(!is_svn_admin_capability(
            "exhibit.repository.initialize_template"
        ));
    }

    #[test]
    fn template_capability_requires_edge_task_context_and_user_identity() {
        let mut options = Options::from_env();
        options.set_mode(crate::app::runtime_mode::AgentMode::Connected);
        let gateway =
            CapabilityGateway::new(options, Arc::new(Mutex::new(LocalWorkerStatus::default())));
        let input = json!({
            "project_id": "prj_1",
            "exhibit_id": "EX-1",
            "engine_type": "Unity3D",
            "template_id": "unity-uniart",
            "svn_username": "alice",
            "prerequisite_task_id": "tsk-edge"
        });
        let direct_error = gateway
            .invoke(
                &InvocationContext::new(InvocationSource::Mcp, "ai-client:test"),
                "exhibit.repository.initialize_template",
                input.clone(),
            )
            .unwrap_err()
            .to_string();
        assert!(direct_error.contains("Edge 前置任务"), "{direct_error}");
        let context =
            InvocationContext::new(InvocationSource::DashboardWorker, "dashboard-user:user-1")
                .with_business_context(json!({
                    "task_id": "tsk-user",
                    "task_type": "exhibit_repository_initialize_template",
                    "source": "dashboard",
                    "prerequisite_task_id": "tsk-edge"
                }));
        let missing_identity = gateway
            .invoke(&context, "exhibit.repository.initialize_template", {
                let mut value = input.clone();
                value["svn_username"] = json!("");
                value
            })
            .unwrap_err()
            .to_string();
        assert!(
            missing_identity.contains("展项模板写入"),
            "{missing_identity}"
        );
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
    fn capability_catalog_cursor_binds_registry_and_filters() {
        let generation = "sha256:registry";
        let filters = "sha256:filters";
        let cursor = format_capability_catalog_cursor(generation, filters, 20);
        assert_eq!(
            parse_capability_catalog_cursor(Some(&cursor), generation, filters).unwrap(),
            20
        );
        assert!(parse_capability_catalog_cursor(Some(&cursor), "sha256:other", filters).is_err());
        assert!(
            parse_capability_catalog_cursor(Some(&cursor), generation, "sha256:other").is_err()
        );
        assert!(parse_capability_catalog_cursor(Some("offset:20"), generation, filters).is_err());
    }

    #[test]
    fn capability_discovery_invalidation_advances_the_epoch() {
        let before = CAPABILITY_DISCOVERY_EPOCH.load(Ordering::Acquire);
        invalidate_capability_discovery();
        let after = CAPABILITY_DISCOVERY_EPOCH.load(Ordering::Acquire);
        assert!(
            after > before,
            "expected a newer discovery epoch, got {after} (before {before})"
        );
    }

    #[test]
    fn plugin_mutation_paths_announce_a_capability_change() {
        let before = CAPABILITY_DISCOVERY_EPOCH.load(Ordering::Acquire);
        crate::capability::plugin::reset_plugin_health("himind-test-absent-plugin").unwrap();
        let after = CAPABILITY_DISCOVERY_EPOCH.load(Ordering::Acquire);
        assert!(
            after > before,
            "plugin health reset must invalidate capability discovery"
        );
    }

    #[test]
    fn replacing_business_catalog_invalidates_gateway_registry_cache() {
        let mut options = crate::Options::from_env();
        options.set_mode(crate::app::runtime_mode::AgentMode::Connected);
        let gateway =
            CapabilityGateway::new(options, Arc::new(Mutex::new(LocalWorkerStatus::default())));
        let context = InvocationContext::local_http();
        let before = gateway
            .list_capabilities(&context)
            .unwrap()
            .into_iter()
            .map(|item| item.id)
            .collect::<std::collections::BTreeSet<_>>();

        gateway.replace_business_catalog_for_test(BusinessCatalogSnapshot::dashboard(
            "cache-invalidation-generation".into(),
            vec![BusinessCapabilityContract {
                id: "business.project.list".into(),
                version: "2.0.0".into(),
                name: "项目列表".into(),
                description: "测试目录能力".into(),
                risk_level: "read_only".into(),
                scope: "business:project:read".into(),
                route: "/api/integrations/ai/business/projects".into(),
                http_method: "GET".into(),
                input_schema: json!({
                    "type": "object",
                    "additionalProperties": false
                }),
                execution_mode: "sync".into(),
                supports_progress: false,
                supports_cancel: false,
                idempotency: "safe".into(),
                approval_required: false,
                retry_policy: "safe".into(),
                concurrency: "parallel".into(),
            }],
        ));

        let after = gateway
            .list_capabilities(&context)
            .unwrap()
            .into_iter()
            .find(|item| item.id == "business.project.list")
            .expect("replacement catalog capability remains visible");
        assert_eq!(after.version, "2.0.0");
        assert!(before.contains("business.project.list"));
    }

    #[test]
    fn health_distinguishes_mcp_stdio_from_mcp_over_local_http() {
        let mut options = crate::Options::from_env();
        options.set_mode(crate::app::runtime_mode::AgentMode::Connected);
        let gateway = CapabilityGateway::new(
            options,
            Arc::new(Mutex::new(LocalWorkerStatus {
                dashboard_worker_state: "offline".to_string(),
                dashboard_worker_reason_code: "connected_agent_app_worker_error".to_string(),
                worker_transport: "local_http".to_string(),
                dashboard_worker_error: "connection failed".to_string(),
                ..LocalWorkerStatus::default()
            })),
        );

        let stdio = gateway.health(&InvocationContext::with_transport(
            InvocationSource::Mcp,
            InvocationTransport::Stdio,
            "stdio-client",
        ));
        assert_eq!(stdio["runtime_schema_version"], 1);
        assert_eq!(
            stdio["dashboard_worker_online_semantics"],
            "legacy_boolean_use_expected_state"
        );
        assert_eq!(stdio["mcp_transport"], "stdio");
        assert_eq!(stdio["dashboard_worker_expected"], false);
        assert_eq!(stdio["dashboard_worker_state"], "not_applicable");
        assert_eq!(stdio["capabilities"], 0);
        assert_eq!(stdio["capabilities_cached"], false);

        let http = gateway.health(&InvocationContext::with_transport(
            InvocationSource::Mcp,
            InvocationTransport::LocalHttp,
            "http-client",
        ));
        assert_eq!(http["mcp_transport"], "local_http");
        assert_eq!(http["dashboard_worker_expected"], true);
        assert_eq!(http["dashboard_worker_state"], "offline");
        assert_eq!(
            http["dashboard_worker_reason_code"],
            "connected_agent_app_worker_error"
        );
        assert_eq!(http["capabilities_cached"], false);
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
    fn remote_approval_sync_is_reserved_for_dashboard_provider() {
        let mut local_plugin = registration(
            "short.video.project.create",
            "创建短视频项目",
            "在本机项目目录创建短视频项目",
            "local_write",
            json!({"type":"object"}),
            CapabilityHandler::PluginCapability("short.video.project.create".into()),
        );
        assert!(!local_plugin.descriptor.dashboard_provider);
        assert!(!should_sync_remote_approval(
            crate::app::runtime_mode::AgentMode::Connected,
            &local_plugin.descriptor
        ));

        local_plugin.descriptor.dashboard_provider = true;
        assert!(should_sync_remote_approval(
            crate::app::runtime_mode::AgentMode::Connected,
            &local_plugin.descriptor
        ));
        assert!(!should_sync_remote_approval(
            crate::app::runtime_mode::AgentMode::Independent,
            &local_plugin.descriptor
        ));
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
    fn exhibit_route_id_guard_does_not_redefine_plugin_arguments() {
        let plugin = registration(
            "plugin.exhibit.lookup",
            "插件展项查询",
            "插件自定义展项参数",
            "read_only",
            json!({
                "type": "object",
                "properties": {"exhibit_id": {"type": "string"}},
                "required": ["exhibit_id"]
            }),
            CapabilityHandler::PluginCapability("plugin.exhibit.lookup".into()),
        );
        assert!(validate_exhibit_route_id_input(
            &plugin.descriptor,
            &json!({"exhibit_id": "EX-0021"})
        )
        .is_ok());

        let dashboard = dashboard_business_registration(
            "business.exhibit.get",
            "读取展项",
            "读取展项",
            "read_only",
            json!({
                "type": "object",
                "properties": {"exhibit_id": {"type": "string"}},
                "required": ["exhibit_id"]
            }),
            CapabilityHandler::DashboardExhibitContext,
        );
        let error = validate_exhibit_route_id_input(
            &dashboard.descriptor,
            &json!({"exhibit_id": "EX-0021"}),
        )
        .expect_err("Dashboard exhibit display id must be rejected");
        assert!(error.to_string().contains("EXHIBIT_ROUTE_ID_REQUIRED"));
    }

    #[test]
    fn authoring_tool_workspace_guard_accepts_any_real_session_workspace() {
        let root = std::env::temp_dir().join(format!(
            "himind-authoring-workspace-guard-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let requested = root.join("session-a");
        std::fs::create_dir_all(&requested).unwrap();

        // 多会话并发下每个会话自带工作区，任意真实目录都应放行，
        // 不再要求它等于某个全局工作区。
        validate_extension_tool_workspace(&json!({
            "workspace_root": requested.to_string_lossy()
        }))
        .unwrap();

        // 真正危险的输入仍然要被挡住：不存在的目录、Agent 自身目录。
        let missing = root.join("does-not-exist");
        let error = validate_extension_tool_workspace(&json!({
            "workspace_root": missing.to_string_lossy()
        }))
        .unwrap_err()
        .to_string();
        assert!(error.contains("extension_workspace_invalid"), "{error}");

        let managed = validate_extension_tool_workspace(&json!({
            "workspace_root": crate::store::paths::agent_home().to_string_lossy()
        }))
        .unwrap_err()
        .to_string();
        assert!(managed.contains("extension_workspace_invalid"), "{managed}");

        let _ = std::fs::remove_dir_all(&root);
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
        options.set_mode(crate::app::runtime_mode::AgentMode::Independent);
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
    fn migrated_dashboard_long_tasks_report_progress_and_cancel_contracts() {
        let handlers = [
            (
                "inner_admin.sync_exhibits",
                CapabilityHandler::InnerAdminSyncExhibits,
            ),
            ("upload.code", CapabilityHandler::UploadCode),
            ("upload.placeholder", CapabilityHandler::UploadPlaceholder),
            ("storage.smb.upload", CapabilityHandler::SmbUpload),
            (
                "exhibit.repository.import_local",
                CapabilityHandler::SvnExhibitRepositoryImportLocal,
            ),
        ];
        for (id, handler) in handlers {
            let item = registration(id, id, id, "network_write", json!({}), handler);
            assert_eq!(item.descriptor.execution_mode, "long_running", "{id}");
            assert!(item.descriptor.supports_progress, "{id}");
            assert!(item.descriptor.supports_cancel, "{id}");
        }
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
        options.set_mode(crate::app::runtime_mode::AgentMode::Independent);
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
        assert!(error.contains("仅由 Edge Worker 执行"));
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
        for capability_id in [
            "svn.user.provision",
            "project.repository.create",
            "exhibit.repository_path.create",
            "project.repository.acl.apply",
        ] {
            assert!(
                !visible.iter().any(|item| item.id == capability_id),
                "central SVN capability must not be exposed: {capability_id}"
            );
        }
        assert!(!visible
            .iter()
            .any(|item| item.id == "exhibit.repository.initialize_template"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn central_svn_capability_invocation_fails_before_any_approval_or_execution() {
        let mut options = Options::from_env();
        options.set_mode(crate::app::runtime_mode::AgentMode::Connected);
        let gateway =
            CapabilityGateway::new(options, Arc::new(Mutex::new(LocalWorkerStatus::default())));
        let error = gateway
            .invoke(
                &InvocationContext::new(InvocationSource::Mcp, "ai-client:test"),
                "project.repository.create",
                json!({}),
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("仅由 Edge Worker 执行"), "{error}");
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
        options.set_mode(crate::app::runtime_mode::AgentMode::Connected);
        let gateway =
            CapabilityGateway::new(options, Arc::new(Mutex::new(LocalWorkerStatus::default())));
        let visible = gateway
            .list_capabilities(&InvocationContext::new(
                crate::capability::types::InvocationSource::LocalHttp,
                "local-user",
            ))
            .unwrap();
        assert!(!visible
            .iter()
            .any(|item| { is_svn_admin_capability(&item.id) }));
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
        options.set_mode(crate::app::runtime_mode::AgentMode::Independent);
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
    fn capability_catalog_cursor_is_bound_to_snapshot_and_filters() {
        let generation = "sha256:generation";
        let filter = capability_catalog_filter_generation("plugin", "", "", "", "");
        let cursor = format_capability_catalog_cursor(generation, &filter, 20);
        assert_eq!(
            parse_capability_catalog_cursor(Some(&cursor), generation, &filter).unwrap(),
            20
        );
        assert!(
            parse_capability_catalog_cursor(Some(&cursor), "sha256:changed", &filter)
                .unwrap_err()
                .to_string()
                .contains("stale")
        );
        let other_filter = capability_catalog_filter_generation("business", "", "", "", "");
        assert!(
            parse_capability_catalog_cursor(Some(&cursor), generation, &other_filter)
                .unwrap_err()
                .to_string()
                .contains("stale")
        );
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
        options.set_mode(crate::app::runtime_mode::AgentMode::Connected);
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
    fn extension_development_workspace_accepts_each_sessions_own_root() {
        let root = std::env::temp_dir().join(format!(
            "himind-extension-workspace-boundary-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let workspace = root.join("workspace");
        let sibling = root.join("sibling-workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();

        // 同一进程服务多个 DSH 工作区会话：每个会话在自己的目录里创作，
        // 两个目录都是合法的，互不构成越界。
        validate_extension_workspace_root(workspace.to_str().unwrap()).unwrap();
        validate_extension_workspace_root(sibling.to_str().unwrap()).unwrap();

        // 只有不存在或指向 Agent 自身目录的输入才被拒绝。
        let error = validate_extension_workspace_root(root.join("missing").to_str().unwrap())
            .unwrap_err()
            .to_string();
        assert!(error.contains("extension_workspace_invalid"), "{error}");
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
