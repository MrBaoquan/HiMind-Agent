import { invoke as tauriInvoke } from '@tauri-apps/api/core';

export type AgentStatus = {
    version: string;
    dashboard_base?: string;
    dashboard_worker_online: boolean;
    dashboard_worker_error?: string;
    dashboard_agent_id?: string;
    dashboard_worker_state?: 'online' | 'connecting' | 'offline' | 'not_applicable' | 'unknown' | string;
    dashboard_worker_expected?: boolean;
    dashboard_worker_reason_code?: string;
    mcp_transport?: 'local_http' | 'stdio' | 'tauri' | 'cli' | 'internal' | 'unknown' | string;
    local_service_expected?: boolean;
    local_port?: number;
    mode?: string;
    effective_mode?: string;
    pending_mode?: string;
    requires_restart?: boolean;
    dashboard_enabled?: boolean;
    control_plane?: {
        kind: 'none' | 'dashboard' | string;
        enabled: boolean;
        available?: boolean;
        worker_state?: string;
        worker_expected?: boolean;
        worker_reason_code?: string;
    };
    login_account?: string;
    login_label?: string;
    profile?: string;
    current_task?: CurrentTaskStatus | null;
};

export type AgentModeSettings = {
    mode: 'connected' | 'independent' | string;
    effective_mode: 'connected' | 'independent' | string;
    pending_mode: 'connected' | 'independent' | string;
    dashboard_enabled: boolean;
    requires_restart: boolean;
};

export type ProjectionSyncStatus = {
    dashboard_enabled: boolean;
    state: 'local_only' | 'synced' | 'pending' | 'attention' | string;
    total: number;
    pending: number;
    retrying: number;
    projected: number;
    dead_letter: number;
    oldest_pending_at: string;
    last_error: string;
};

export type CurrentTaskStatus = {
    task_id: string;
    task_type: string;
    execution_id: string;
    status: 'running' | string;
};

export type AgentTaskHistoryItem = {
    id: string;
    task_type: string;
    status: string;
    progress: number;
    detail?: string | null;
    error?: string | null;
    created_at: string;
    started_at?: string | null;
    finished_at?: string | null;
    updated_at: string;
};

export type CatalogPage<T> = {
    items: T[];
    total: number;
    page: number;
    page_size: number;
};

export type AgentUpdateStatus = {
    status: 'idle' | 'checking' | 'available' | 'downloading' | 'ready' | 'installing' | 'failed' | 'rolled_back' | string;
    current_version: string;
    channel: string;
    source: 'dashboard' | 'github' | string;
    available_version: string;
    release_id: string;
    file_name: string;
    package_type: 'directory-zip';
    size_bytes: number;
    mandatory: boolean;
    min_supported_version: string;
    release_notes: string;
    downloaded_bytes: number;
    progress_percent: number;
    last_checked_at: number;
    last_error: string;
    auto_check: boolean;
    auto_download: boolean;
};

export type ApprovalItem = {
    id: string;
    request_type: string;
    title: string;
    description: string;
    timeout_seconds?: number;
    remaining_seconds?: number;
    created_at?: string;
};

export type ApprovalFact = {
    schema_version: number;
    id: string;
    request_type: string;
    title: string;
    description: string;
    status: 'pending' | 'approved' | 'rejected' | 'expired' | 'interrupted';
    created_at_unix: number;
    expires_at_unix: number;
    resolved_at_unix?: number;
    resolution_reason?: string;
};

export type UnityEditorSettings = {
    unity_editor_path: string;
    workflow_default_path: string;
    discovered_path?: string;
    source: 'agent' | 'environment' | 'discovered' | 'unset';
    valid: boolean;
};

export type ApprovalSettings = {
  timeout_seconds: number;
  auto_start: boolean;
  rules?: Record<string, string>;
  profile?: 'strict' | 'balanced' | 'relaxed' | 'trusted' | 'full_access' | 'silent_deny' | 'focus' | string;
  notification_mode?: 'popup' | 'tray' | 'inbox' | string;
  owner_user_id?: string;
  agent_id?: string;
  binding_updated_at?: number;
  risk_acknowledged_at?: number;
  risk_acknowledged?: boolean;
  risk_acknowledged_duration_seconds?: number;
  risk_acknowledged_remaining_seconds?: number;
  effective_modes?: {
    read: 'manual' | 'auto_approve' | 'auto_deny';
    write: 'manual' | 'auto_approve' | 'auto_deny';
    high_risk: 'manual' | 'auto_approve' | 'auto_deny';
    system: 'manual' | 'auto_approve' | 'auto_deny';
  };
  editors?: UnityEditorSettings;
};

export type RemoteExecutionSettings = {
    enabled: boolean;
    /**
     * `full_access` is the persisted compatibility value for an unrestricted
     * remote runtime, not the Agent approval-profile setting of the same name.
     */
    access_mode: 'exhibit_linked' | 'full_access';
    default_provider: 'himind.builtin' | 'auto' | 'personal.codex' | 'personal.github-copilot';
};

export type RemoteClientVendor = 'sunlogin' | 'todesk';

export type RemoteClientStatus = {
    vendor: RemoteClientVendor;
    name: string;
    available: boolean;
    configured_path?: string;
    configured_by?: 'manual' | 'auto' | string;
    configured_valid?: boolean;
    resolved_path?: string;
    source?: string;
    auto_configured?: boolean;
};

export type RemoteClientOverview = {
    items: RemoteClientStatus[];
    settings_file?: string;
};

export type WorkflowStep = {
    id: string;
    title: string;
    /** capability | runtime | manual | loop | provider_defined */
    kind?: string;
    capability_id: string;
    execution_mode: string;
    risk_level: string;
    approval_required: boolean;
    depends_on: string[];
    runtime?: {
        provider?: string;
        prompt?: string;
        input_artifacts?: string[];
        tool_policy?: string;
        timeout_seconds?: number;
    } | null;
    loop?: {
        max_iterations: number;
        pause_for_feedback?: boolean;
        continue_when?: WorkflowCondition | null;
        exit_when?: WorkflowCondition | null;
        steps: WorkflowStep[];
    } | null;
    when?: WorkflowCondition | null;
    fail_when?: WorkflowCondition | null;
    on_failure?: string;
};

export type WorkflowCondition = {
    operator: string;
    path?: string;
    value?: unknown;
    conditions?: WorkflowCondition[];
};

export type WorkflowArtifact = {
    id: string;
    artifact_type: string;
    name: string;
    schema: string;
    required: boolean;
    validation?: string;
    max_bytes?: number;
};

export type WorkflowPackage = {
    schema_version: string;
    id: string;
    version: string;
    name: string;
    description: string;
    min_agent_version: string;
    execution_policy?: 'strict' | 'segmented' | 'flexible' | string;
    capabilities: string[];
    dependencies: {
        skills: string[];
        plugins: string[];
        connectors: string[];
        runtimes: string[];
    };
    steps: WorkflowStep[];
    artifacts: WorkflowArtifact[];
    entrypoints?: Array<{ id: string; at_step: string; label?: string; requires?: string[]; produces?: string[] }>;
    default_entrypoint?: string;
    exits?: Array<{ id: string; at_step: string; label?: string; requires?: string[]; produces?: string[] }>;
    default_exitpoint?: string;
    ui: { mode: 'standard' | 'declarative' | 'custom' | string; entry: string; surfaces: string[] };
    supported_runtimes: string[];
};

export type WorkflowView = {
    schema_version: string;
    title: string;
    sections: Array<{ id: string; title: string; fields?: WorkflowViewField[]; artifacts?: string[] }>;
    actions: string[];
};

export type WorkflowViewField = string | {
    id: string;
    label?: string;
    type?: 'text' | 'textarea' | 'number' | 'boolean' | 'select' | 'list' | 'json' | 'credential' | string;
    required?: boolean;
    default?: unknown;
    options?: string[];
    placeholder?: string;
    target?: string;
    /** 声明输入控件类型，当前支持 directory（带目录选择按钮）。 */
    picker?: string;
    /** 字段级说明，用来解释默认值行为。 */
    hint?: string;
    /** 栅格占位，full 表示整行。 */
    span?: string;
};

export type WorkflowLocalRun = {
    run_id: string;
    interaction_id: string;
    status: 'queued' | 'running' | 'waiting' | 'succeeded' | 'failed' | 'canceled' | string;
    current_step_id: string;
    runtime_provider: string;
    workspace_ref: string;
    steps: Array<{
        step_id: string;
        title: string;
        status: string;
        capability_id: string;
        attempt: number;
        started_at: string;
        finished_at: string;
        error: string;
    }>;
    approvals: Array<{ approval_id: string; capability_id: string; risk_level: string; status: string; owner: string }>;
    artifacts: Array<{ artifact_id: string; artifact_type: string; name: string; uri: string; sha256: string; size_bytes: number }>;
    error: string;
    created_at: string;
    updated_at: string;
};

export type WorkflowCenterItem = {
    package: WorkflowPackage;
    enabled: boolean;
    previous_version: string;
    package_digest: string;
    source: string;
    installed_at: string;
    updated_at: string;
    view: WorkflowView | null;
    metrics: {
        total_runs: number;
        terminal_runs: number;
        succeeded_runs: number;
        failed_runs: number;
        canceled_runs: number;
        active_runs: number;
        completion_rate: number;
        average_duration_seconds: number;
        total_input_tokens: number;
        total_output_tokens: number;
        estimated_cost: number;
        rework_runs: number;
        retry_count: number;
        approval_count: number;
        feedback_wait_count: number;
        last_run_at: string;
    };
};

/**
 * 平台级定时任务：一条计划描述“什么时候、对什么目标做什么”。
 * 目前 `kind` 只有 `workflow`，新增目标类型由平台侧扩展，前端按 kind 渲染。
 */
export type ScheduleExecution = {
    entrypoint?: string;
    exitpoint?: string;
};

export type Schedule = {
    id: string;
    kind: string;
    target_id: string;
    /** Workflow 启动预设来源；input 是保存时的不可变快照。 */
    preset_id?: string;
    input: Record<string, unknown>;
    execution?: ScheduleExecution;
    cron: string;
    enabled: boolean;
    next_run_at: string;
    last_run_at: string;
    last_run_id: string;
    last_status: string;
    last_error: string;
    created_at: string;
    updated_at: string;
};

export type ScheduleList = {
    now: string;
    store_path: string;
    timezone: string;
    target_kinds: string[];
    schedules: Schedule[];
};

/** 一次技能运行：结果落在 Agent 的 skill-runs 记录里，可回看、可定位。 */
export type SkillRun = {
    run_id: string;
    skill_id: string;
    skill_name: string;
    skill_version: string;
    schedule_id: string;
    task: string;
    workspace: string;
    status: 'running' | 'succeeded' | 'failed' | string;
    started_at: string;
    finished_at: string;
    duration_seconds: number;
    timeout_seconds: number;
    output_path: string;
    output_chars: number;
    output_preview: string;
    error: string;
};

export type SkillRunList = {
    root: string;
    total: number;
    runs: SkillRun[];
};

/**
 * 启动预设：一套可复用的启动参数。同一个工作流针对不同工作区启动时，
 * 只需替换 workspace_root 这类字段，其余沿用预设。
 */
export type WorkflowRunPreset = {
    id: string;
    workflow_id: string;
    label: string;
    input: Record<string, unknown>;
    entrypoint: string;
    exitpoint: string;
    created_at: string;
    updated_at: string;
};

export type WorkflowRunPresetInput = {
    id?: string;
    workflow_id: string;
    label?: string;
    input?: Record<string, unknown>;
    entrypoint?: string;
    exitpoint?: string;
};

export type WorkflowInteractionKind = 'approval' | 'feedback' | 'form' | 'evidence' | 'external_wait' | string;

/**
 * Workflow Runner 等待态的稳定 UI View Model。
 * 事实仍来自 Runtime Event；页面不再直接猜测 payload 字段。
 */
export type WorkflowInteractionRequest = {
    schema_version: 'interaction_request.v1' | string;
    id: string;
    run_id: string;
    step_id: string;
    kind: WorkflowInteractionKind;
    title: string;
    description: string;
    required_action: 'approve_or_reject' | 'submit_feedback' | 'submit_form' | 'submit_evidence' | 'inspect_run' | string;
    status: 'pending' | 'resolved' | 'expired' | string;
    source_event_id?: string;
    created_at?: string;
    risk_level?: string;
    schema?: Record<string, unknown> | null;
    metadata?: Record<string, unknown>;
};

export type ScheduleInput = {
    id?: string;
    kind?: string;
    target_id: string;
    preset_id?: string;
    cron: string;
    input?: Record<string, unknown>;
    execution?: ScheduleExecution;
    enabled?: boolean;
};

export type WorkflowCenterSnapshot = {
    workflows: WorkflowCenterItem[];
    runs: Array<{
        workflow_id: string;
        workflow_name: string;
        workflow_version: string;
        business_stage: string;
        current_step_title: string;
        waiting_kind: WorkflowInteractionKind | '';
        waiting_reason?: string;
        required_action?: string;
        interaction_request?: WorkflowInteractionRequest | null;
        project_root: string;
        workspace_root: string;
        app_id: string;
        run: WorkflowLocalRun;
        projection_count: number;
        projection_status: string;
    }>;
    catalog?: WorkflowCatalogItem[];
    catalog_error?: string;
};

export type ConnectorStateItem = {
    id: string;
    name: string;
    version: string;
    availability: string;
    credential_ownership: string;
    enabled: boolean;
    revoked: boolean;
    source: string;
    remote_revision: number;
    reason: string;
    updated_at: string;
    credential_count: number;
};

export type WorkflowRunSnapshot = {
    run: WorkflowLocalRun;
    interaction: unknown;
    events: Array<{
        event_id: string;
        step_id: string;
        capability_id: string;
        sequence: number;
        occurred_at: string;
        provider: string;
        event_type: string;
        payload: unknown;
    }>;
    projections: Array<{ id: number; status: string; attempts: number; last_error: string; payload: unknown }>;
    interaction_request?: WorkflowInteractionRequest | null;
    workflow: { package: WorkflowPackage; enabled: boolean; view: WorkflowView | null } | null;
};

export type WorkflowRunVerification = {
    workflow_id: string;
    run_id: string;
    package_digest: string;
    signature_key_id: string;
    signature_algorithm: string;
    candidate_id: string;
    commit_sha: string;
    tree_digest: string;
    artifacts: Array<{
        artifact_id: string;
        artifact_type: string;
        uri: string;
        sha256: string;
        size_bytes: number;
        schema_validation: string;
        candidate_bound: boolean;
    }>;
};

export type WorkflowPreflight = {
    ready: boolean;
    package_id: string;
    package_version: string;
    agent_version: string;
    capabilities: Array<{ id: string; available: boolean; source: string; availability: string }>;
    skills: Array<{ id: string; available: boolean; version: string; scope: string }>;
    runtimes: Array<{ id: string; available: boolean; status: string; version: string; network_isolated: boolean; tool_access: string }>;
    connectors: Array<{
        id: string;
        available: boolean;
        availability: string;
        credential_ownership: string;
        health_check: string;
        health_target: string;
        health_status: string;
        health_message: string;
        credentials: Array<{
            handle: string;
            target: string;
            kind: 'file_path' | 'secret' | string;
            required: boolean;
            configured: boolean;
            configured_connector_id: string;
        }>;
    }>;
    tools: Array<{ id: string; available: boolean; required: boolean; resolved_path: string }>;
    diagnostics: Array<{
        severity: 'blocker' | 'warning' | string;
        code: string;
        stage: string;
        message: string;
        remediation: string;
        retryable: boolean;
    }>;
    blockers: string[];
    warnings: string[];
};

export type ConnectorCredentialSummary = {
    handle: string;
    connector_id: string;
    kind: 'file_path' | 'secret' | string;
    updated_at: string;
};

export type BuiltinAIRuntimeStatus = {
    provider: 'himind.builtin' | string;
    status: 'ready' | 'unavailable' | string;
    version: string;
    compatible: boolean;
    compatible_with_agent?: boolean;
    min_agent_version?: string;
    max_agent_version?: string;
    capabilities?: string[];
    message: string;
    diagnostics: {
        engine_id: string;
        executable_path: string;
        contract_version: number;
        update_mode: string;
    };
};

export type AcpRuntimeProfile = {
    provider_id: string;
    display_name: string;
    executable: string;
    args: string[];
    version: string;
    permission_policy: 'deny' | 'allow_once' | 'prompt' | string;
    enabled: boolean;
};

export type AcpRuntimeProfileSnapshot = {
    profiles: AcpRuntimeProfile[];
    providers: Array<{
        provider: string;
        version?: string;
        status: 'ready' | 'unavailable' | 'unsupported' | string;
        capabilities?: Record<string, unknown>;
    }>;
};

export type AcpRuntimeProfileInput = {
    providerId: string;
    displayName: string;
    executable: string;
    args: string[];
    version: string;
    permissionPolicy: 'deny' | 'allow_once' | 'prompt';
    enabled: boolean;
};

export type BuiltinAIRuntimeInstallationStatus = {
    state: 'idle' | 'working' | 'ready' | 'failed' | string;
    operation: 'none' | 'install' | 'update' | 'repair' | 'local' | 'uninstall' | string;
    stage: 'idle' | 'resolving' | 'downloading' | 'verifying' | 'installing' | 'uninstalling' | 'ready' | 'failed' | string;
    progress_percent: number;
    message: string;
    error: string;
    runtime: BuiltinAIRuntimeStatus;
    update_available: boolean;
    available_version: string;
    release_notes: string;
    mandatory_update: boolean;
};

export type BuiltinAIToolContextSummary = {
    skills: number;
    mcp_services: number;
};

export type BuiltinAiModelSyncResult = {
    status: 'unchanged' | 'updated' | 'restarted' | 'restart_required' | string;
    model_count: number;
    restarted: boolean;
    session_url: string;
};

export type BuiltinAIRuntimeSession = {
    id: string;
    conversation_id?: string;
    owner_user_id: string;
    agent_id: string;
    provider: string;
    provider_session_id: string;
    workspace_ref?: string;
    status: string;
    generation: number;
    capabilities: Record<string, boolean>;
    metadata: Record<string, unknown>;
    last_heartbeat_at: string;
    created_at: string;
    updated_at: string;
};

export type BuiltinAIRuntimeActivity = {
    session: BuiltinAIRuntimeSession;
    conversation?: { id: string; title: string; status: string; updated_at: string };
    endpoints?: { channel: string; conversation_type: string; status: string }[];
    latest_turn?: { role: string; content: string; surface?: string; created_at: string };
};

export type BuiltinAIMcpServer = {
    server_name: string;
    display_name: string;
    transport: 'stdio' | 'streamable-http';
    command: string;
    args: string[];
    env: Record<string, string>;
    cwd: string;
    url: string;
    headers: Record<string, string>;
    tool_call_timeout_ms: number;
    fail_on_startup_error: boolean;
    reconnect: boolean;
    enabled: boolean;
};

export type LoginState = {
    status: string;
    account?: string;
};

export type SvnConnection = {
    id: string;
    name: string;
    base_url: string;
    username: string;
    provider: 'svn' | 'svnadmin_v2';
    credentials_configured: boolean;
    status?: 'configured' | 'ready' | 'invalid' | 'unreachable' | string;
    last_error?: string;
};

export type SvnConnectionInput = {
    username: string;
    password: string;
};

export type SvnConnectionTest = {
    connection_id: string;
    provider: string;
    status: string;
    authenticated: boolean;
    revision?: string;
    message?: string;
};

export type LogItem = {
    time?: string;
    timestamp?: number;
    level?: string;
    message?: string;
};

export type DiagnosticsExportResult = {
    canceled: boolean;
    path?: string;
};

export type PluginRegistry = {
    registry_ready: boolean;
    registry_dir?: string;
    external_runtime?: string;
    total?: number;
    items?: PluginItem[];
};

export type ExtensionDesiredItem = {
    product_id: string;
    asset_key: string;
    asset_kind: 'plugin' | 'skill' | string;
    name: string;
    desired_state: 'absent' | 'present' | 'optional' | string;
    desired_version?: string;
    desired_enabled?: boolean;
    intent: 'required' | 'recommended' | 'optional' | string;
    management: 'user_managed' | 'organization_managed' | 'builtin' | string;
    install_mode?: 'prompt' | 'silent' | string;
    assignment_id?: string;
    source?: 'marketplace' | 'organization' | 'system' | string;
    reason?: string;
    allow_disable?: boolean;
    allow_uninstall?: boolean;
    on_scope_exit?: string;
};

export type ExtensionDesiredState = {
    generation: string;
    reconcile_interval_seconds: number;
    items: ExtensionDesiredItem[];
};

export type ExtensionSourceConfig = {
    id: string;
    name: string;
    kind?: 'github' | 'local';
    repository: string;
    reference: string;
    catalog_path: string;
    enabled: boolean;
    auto_update: boolean;
    verification: 'required' | 'optional';
    upstream_repository?: string;
};

export type ExtensionSourceSettings = {
    schema_version: number;
    sources: ExtensionSourceConfig[];
    acquisitions?: Record<string, 'local' | 'remote'>;
};

/// 分发单元内某个已安装制品的来源侧，用于判断本机生效版本是否与所选来源一致。
export type ExtensionUnitInstallation = {
    asset_kind: 'plugin' | 'skill' | string;
    asset_id: string;
    version: string;
    source_id: string;
    sha256?: string;
    side: 'local' | 'remote' | 'development' | string;
};

/// 取用侧目录当前提供的制品，用于与 installed 对比得出可更新项。
export type ExtensionUnitAsset = {
    asset_kind: 'plugin' | 'skill' | string;
    asset_id: string;
    name: string;
    version: string;
    source_id: string;
    source_kind: 'local' | 'github' | string;
    artifact_url: string;
    sha256: string;
    signature_key_id: string;
    signature_algorithm: string;
    channel: string;
};

/// 分发单元的取用侧：本地开发工作区是默认的最新权威，GitHub 分发源为兜底。
export type ExtensionSourceAcquisition = 'local' | 'remote';

export type ExtensionDistributionUnit = {
    unit_key: string;
    name: string;
    acquisition: ExtensionSourceAcquisition;
    local_source_id?: string | null;
    remote_source_id?: string | null;
    repository: string;
    local_root?: string | null;
    plugin_count: number;
    skill_count: number;
    workflow_count: number;
    state: 'ready' | 'empty' | string;
    plugin_ids: string[];
    skill_ids: string[];
    workflow_ids: string[];
    project_ids: string[];
    installed: ExtensionUnitInstallation[];
    assets: ExtensionUnitAsset[];
    /** 非取用侧的情况；单元只有一侧来源时为 null。 */
    other_side?: ExtensionUnitOtherSide | null;
};

/**
 * 单元里另一侧的可用性与版本落差。取用侧决定市场和安装看到的内容，另一侧的更高
 * 版本不会自动顶上来，界面据此提示「远端已有更新，可切换取用侧」。
 */
export type ExtensionUnitOtherSide = {
    side: 'local' | 'remote' | string;
    available: boolean;
    newer_count: number;
};

export type ExtensionUnitInstallReport = {
    unit_key: string;
    acquisition: ExtensionSourceAcquisition;
    plugins: ExtensionUnitAsset[];
    skills: ExtensionUnitAsset[];
    workflows: ExtensionUnitAsset[];
    errors: string[];
};

export type ExtensionSourceNotice = {
    reason: string;
    items: string[];
};

export type ExtensionSourceStatus = {
    source: ExtensionSourceConfig;
    state: 'ready' | 'unavailable' | string;
    plugin_count: number;
    skill_count: number;
    workflow_count: number;
    generation: string;
    using_cache: boolean;
    error: string;
    notices?: ExtensionSourceNotice[];
};

export type ExtensionFeaturePack = {
    id: string;
    name: string;
    plugin_ids: string[];
    skill_ids: string[];
};

export type WorkflowCatalogItem = {
    workflow_id: string;
    name: string;
    description: string;
    author_name: string;
    categories: string[];
    version: string;
    release_notes: string;
    published_at: string;
    min_agent_version: string;
    capability_ids: string[];
    channel: string;
    product_id?: string;
    release_id?: string;
    artifact_id: string;
    file_name: string;
    file_size: number;
    sha256: string;
    signature: string;
    signature_key_id: string;
    signature_algorithm: string;
    download_url: string;
    source: string;
    assignment: string;
    management: string;
    install_mode: string;
    organization_reason: string;
    managed: boolean;
    allow_disable: boolean;
    allow_uninstall: boolean;
    extension_lock?: Record<string, unknown> | null;
};

export type ExtensionSourceSnapshot = {
    plugins: PluginCatalogItem[];
    skills: OrganizationSkillCatalogItem[];
    workflows: WorkflowCatalogItem[];
    feature_packs: ExtensionFeaturePack[];
    sources: ExtensionSourceStatus[];
    units?: ExtensionDistributionUnit[];
};

export type ExtensionProvenance = {
    asset_kind: 'plugin' | 'skill' | string;
    asset_key: string;
    version: string;
    source_id: string;
    repository: string;
    reference: string;
    catalog_path: string;
    artifact_url: string;
    sha256: string;
    signature_key_id: string;
    auto_update: boolean;
};

export type PluginItem = {
    id: string;
    name?: string;
    description?: string;
    release_notes?: string;
    author_name?: string;
    version?: string;
    runtime?: string;
    min_agent_version?: string;
    status?: string;
    enabled?: boolean;
    error?: string;
    development?: boolean;
    path?: string;
    entry?: string;
    entry_modified_at?: number;
    entry_size?: number;
    previous_version?: string;
    rollback_available?: boolean;
    failure_count?: number;
    circuit_open?: boolean;
    governance?: 'required' | 'managed' | 'optional' | 'blocked';
    availability?: 'local' | 'network_service' | 'control_plane' | string;
    source?: string;
    permissions?: string[];
    plugin_dependencies?: SkillPluginDependency[];
    capabilities?: PluginCapability[];
    views?: PluginViewContribution[];
    commands?: { id: string; title?: string }[];
};

export type PluginJsonSchema = {
    type?: string;
    description?: string;
    properties?: Record<string, PluginJsonSchema>;
    required?: string[];
    minimum?: number;
    default?: unknown;
    additionalProperties?: boolean;
};

export type PluginCapability = {
    id: string;
    description?: string;
    input_schema?: PluginJsonSchema;
    risk_level?: string;
};

export type DevelopmentInvocationResult = {
    ok: boolean;
    duration_ms: number;
    result?: unknown;
    error?: string;
};

export type PluginCatalogItem = {
    plugin_id: string;
    name: string;
    description: string;
    author_name?: string;
    categories?: string[];
    review_status?: string;
    governance: 'required' | 'managed' | 'optional' | 'blocked';
    version: string;
    release_notes: string;
    published_at?: string;
    min_agent_version: string;
    channel?: string;
    product_id?: string;
    release_id?: string;
    artifact_id: string;
    file_size: number;
    sha256: string;
    source?: 'marketplace' | 'organization' | 'system' | string;
    assignment?: 'optional' | 'recommended' | 'required' | 'blocked' | string;
    management?: 'user_managed' | 'organization_managed' | 'builtin' | string;
    install_mode?: 'prompt' | 'silent' | string;
    organization_reason?: string;
    managed?: boolean;
    allow_disable?: boolean;
    allow_uninstall?: boolean;
    capability_ids?: string[];
    permissions?: string[];
    view_count?: number;
    plugin_dependencies?: SkillPluginDependency[];
};

export type DashboardIdentityStatus = {
    state: 'not_enrolled' | 'not_authorized' | 'authorized' | 'dashboard_unavailable' | 'requires_login' | 'insufficient_scope' | 'expired' | 'disabled' | 'invalid_local_authorization' | string;
    authorized: boolean;
    online_verified: boolean;
    dashboard_base: string;
    user_name: string;
    user_id: string;
    agent_id: string;
    scopes: string[];
    refresh_expires_at: number;
    last_verified_at: number;
    svn_username: string;
    svn_provisioning_status: string;
    svn_provisioning_error: string;
    error: string;
};

export type DashboardAuthorizationProgress = {
    state: 'idle' | 'starting' | 'pending' | 'authorized' | 'denied' | 'expired' | 'canceled' | 'failed' | string;
    user_code: string;
    verification_uri: string;
    verification_uri_complete: string;
    expires_at: number;
    error: string;
    user_name: string;
    user_id: string;
};

export type McpConnectionTestResult = {
    ok: boolean;
    server_name: string;
    server_version: string;
    protocol_version: string;
    capability_count: number;
    duration_ms: number;
};

export type McpRegistrySnapshot = {
    schema_version: number;
    servers: Array<Record<string, unknown>>;
};

export type McpTargetDescriptor = {
    id: string;
    name: string;
    kind: string;
    detected: boolean;
    detection_message: string;
    config_path: string;
    config_directory: string;
    config_format: string;
    state: string;
    supported_transports: string[];
    supports_auto_configure: boolean;
    supports_skills: boolean;
    skill_client_id: string;
    skill_client_name: string;
    restart_required: boolean;
    manual_snippet: string;
    config_preview: string;
    error: string;
};

export type McpRegistrationPlan = {
    target_id: string;
    action: 'create' | 'update' | 'remove' | 'noop' | 'unsupported' | string;
    write_required: boolean;
    backup_required: boolean;
    restart_required: boolean;
    configured_server_id: string;
    warnings: string[];
};

export type McpTargetOperationResult = {
    target: McpTargetDescriptor;
    changed: boolean;
    backup_path: string;
    message: string;
};

export type McpTargetBatchResult = {
    results: McpTargetOperationResult[];
    failures: Array<{ target_id: string; target_name: string; error: string }>;
    skipped_target_ids: string[];
};

export type McpProbeResult = McpConnectionTestResult & {
    transport: string;
    tool_count: number;
    error_kind: string;
    error: string;
};

export type PluginViewContribution = {
    id: string;
    title: string;
    /** Compact label used by the Agent quick-tools surface. */
    short_title?: string;
    /** Semantic icon key resolved by the Agent host. */
    icon?: string;
    /** Whether this view should be exposed as a quick launch entry. */
    quick_access?: boolean;
    /** Stable ordering hint within the quick-tools surface. */
    order?: number;
    location?: string;
    entry: string;
};

/** A host-owned projection of an installed plugin view for quick launch. */
export type PluginQuickAccessView = {
    plugin_id: string;
    plugin_name: string;
    view_id: string;
    title: string;
    short_title: string;
    icon: string;
    order: number;
};

export type CapabilityItem = {
    id: string;
    name?: string;
    source?: string;
    risk_level?: string;
    description?: string;
    availability?: 'local' | 'network_service' | 'control_plane' | string;
};

export type SkillScope = 'builtin' | 'organization' | 'user';
export type SkillTargetKind = 'global' | 'workspace';

export type SkillWorkspaceStatus = {
    configured: boolean;
    valid: boolean;
    root: string;
    workspace_id: string;
    agents_skills_root: string;
    lock_path: string;
    managed_skill_count: number;
    managed_skills?: Array<{ skill_id: string; version: string; enabled: boolean }>;
    error: string;
};

export type SkillImportResponse = {
    record: SkillRecord;
    clients: Record<string, CodexSkillActionResponse | Record<string, unknown>>;
    deployment: 'current-target' | string;
};

export type DiscoveredProjectSkill = {
    skill_id?: string;
    name: string;
    description: string;
    path: string;
    managed_by_himind: boolean;
    management_mode: 'managed' | 'native' | string;
};

export type SkillCapabilityDependency = {
    id: string;
    required?: boolean;
    min_version?: string;
    max_version?: string;
    provider?: string;
};

export type SkillManifest = {
    id: string;
    name: string;
    author?: string;
    categories?: string[];
    version: string;
    scope: SkillScope;
    description?: string;
    release_notes?: string;
    min_agent_version?: string;
    supported_clients?: string[];
    capabilities?: SkillCapabilityDependency[];
    plugin_dependencies?: SkillPluginDependency[];
    risk_summary?: string;
    contents?: string[];
};

export type SkillRecord = {
    manifest: SkillManifest;
    root: string;
    version_root: string;
    current: boolean;
    previous_version?: string | null;
};

export type SkillDependencyResolution = {
    id: string;
    required: boolean;
    state: string;
    reason?: string | null;
    capability_version?: string | null;
    provider?: string | null;
};

export type SkillReadiness = {
    state: string;
    reasons: string[];
    dependencies: SkillDependencyResolution[];
};

export type SkillCatalogItem = {
    record: SkillRecord;
    readiness: SkillReadiness;
};

export type SkillCatalogResponse = {
    client_id: string;
    agent_version: string;
    store_root: string;
    items: SkillCatalogItem[];
};

export type CodexSkillStatusItem = {
    record: SkillRecord;
    readiness: SkillReadiness;
    rendered_root: string;
    rendered: boolean;
    rendered_valid: boolean;
    client_state: 'not_installed' | 'installed' | 'outdated' | 'modified' | 'managed_elsewhere' | 'blocked' | 'unsupported' | 'failed';
    installed_version?: string | null;
    managing_profile?: string | null;
    available_version: string;
    pinned_version?: string | null;
    update_available?: boolean;
    last_synced_at?: string | null;
    managed_files: string[];
    modified_files: string[];
    available_actions: Array<'install' | 'update' | 'repair' | 'uninstall'>;
};

export type SkillPluginDependency = {
    plugin_id: string;
    required: boolean;
    min_version?: string | null;
};

export type OrganizationSkillCatalogItem = {
    skill_id: string;
    name: string;
    description: string;
    author_name: string;
    categories: string[];
    version: string;
    release_notes: string;
    published_at?: string;
    min_agent_version: string;
    supported_clients: string[];
    capability_ids: string[];
    plugin_dependencies: Array<{ plugin_id: string; required: boolean; min_version?: string }>;
    risk_summary: string;
    channel: string;
    product_id?: string;
    release_id?: string;
    artifact_id: string;
    file_name: string;
    file_size: number;
    sha256: string;
    signature: string;
    signature_key_id: string;
    signature_algorithm: string;
    download_url: string;
    source?: 'marketplace' | 'organization' | 'system' | string;
    assignment?: 'optional' | 'recommended' | 'required' | 'blocked' | string;
    management?: 'user_managed' | 'organization_managed' | 'builtin' | string;
    install_mode?: 'prompt' | 'silent' | string;
    organization_reason?: string;
    managed?: boolean;
    allow_disable?: boolean;
    allow_uninstall?: boolean;
};

export type OrganizationSkillInstallResponse = {
    catalog_item: OrganizationSkillCatalogItem;
    record: SkillRecord;
    codex: CodexSkillActionResponse;
    github_copilot?: CodexSkillActionResponse;
    workbuddy?: CodexSkillActionResponse;
    clients?: Record<string, CodexSkillActionResponse>;
};

export type SkillPluginInstallAction = {
    plugin_id: string;
    plugin_name: string;
    plugin_description: string;
    required: boolean;
    current_version: string;
    target_version: string;
    action: 'satisfied' | 'install' | 'update' | 'blocked' | 'unavailable';
    reason: string;
};

export type PluginInstallPlan = {
    plugin: PluginCatalogItem;
    dependency_actions: Array<SkillPluginInstallAction & { requested_by: string }>;
    blocked_reasons: string[];
    ready: boolean;
};

export type SkillInstallPlan = {
    skill: OrganizationSkillCatalogItem;
    plugin_actions: SkillPluginInstallAction[];
    blocked_reasons: string[];
    ready: boolean;
};

export type AuthoringSkillDraftInput = {
    id: string;
    name: string;
    author: string;
    categories: string[];
    version: string;
    description: string;
    release_notes: string;
    min_agent_version: string;
    supported_clients: string[];
    capabilities: SkillCapabilityDependency[];
    plugin_dependencies: SkillPluginDependency[];
    risk_summary: string;
    readme: string;
    files?: Record<string, string>;
};

export type AuthoringPluginDraft = {
    manifest: {
        id: string;
        name: string;
        author?: string;
        description?: string;
        release_notes?: string;
        version: string;
        runtime?: string;
        capabilities?: PluginCapability[];
        permissions?: string[];
        plugin_dependencies?: SkillPluginDependency[];
    };
    candidate_path: string;
    candidate_sha256: string;
    development_path?: string | null;
    workspace_path?: string | null;
    source?: string;
    revision_of?: string | null;
    parent_submission_id?: string | null;
    tested_at?: string | null;
    confirmed_at?: string | null;
    submitted_at?: string | null;
    dashboard_submission_id?: string | null;
    updated_at: string;
};

export type AuthoringWorkflowDraft = {
    id?: string;
    product_key?: string;
    product_name?: string;
    package_id: string;
    version: string;
    name: string;
    manifest: {
        id: string;
        name: string;
        description: string;
        version: string;
        release_notes?: string;
        capabilities?: string[];
        plugin_dependencies?: SkillPluginDependency[];
    };
    source_root: string;
    candidate_path: string;
    candidate_sha256: string;
    state: 'draft' | 'candidate' | 'tested' | 'confirmed' | 'submitted';
    status?: 'submitted' | 'approved' | 'changes_requested' | 'rejected' | 'superseded';
    review_status?: string;
    review_note?: string;
    release_notes?: string;
    release_status?: string;
    role?: 'owner' | 'contributor';
    sha256?: string;
    test_report?: Record<string, unknown>;
    created_at: string;
    updated_at: string;
    tested_at?: string | null;
    confirmed_at?: string | null;
    submitted_at?: string | null;
    dashboard_submission_id?: string | null;
    dashboard_draft_id?: string | null;
    lock?: {
        schema_version: string;
        root: { kind: string; id: string; version: string; sha256: string };
        dependencies: Array<{ kind: string; id: string; version: string; sha256: string; source_id?: string; required: boolean }>;
        generated_at: string;
    } | null;
    lock_path?: string | null;
};

export type PluginSubmissionStatus = {
    id: string;
    product_key: string;
    name: string;
    version: string;
    status: 'submitted' | 'approved' | 'changes_requested' | 'rejected' | 'superseded';
    review_status: string;
    review_note?: string;
    release_notes?: string;
    artifact_id: string;
    release_id: string;
    release_status?: 'draft' | 'published' | 'revoked' | string;
    parent_release_id?: string;
    revision_of_version?: string;
    source_type?: string;
    source_repository?: string;
    source_branch?: string;
    source_subdirectory?: string;
    source_commit?: string;
    role?: 'owner' | 'contributor';
    sha256: string;
    updated_at: string;
};

export type AuthoringSkillDraft = {
    manifest: SkillManifest;
    readme: string;
    files?: Record<string, string>;
    candidate_path: string;
    candidate_sha256: string;
    workspace_path?: string | null;
    source?: string;
    revision_of?: string | null;
    parent_submission_id?: string | null;
    tested_at?: string | null;
    confirmed_at?: string | null;
    submitted_at?: string | null;
    dashboard_draft_id?: string | null;
    codex_target?: string | null;
    client_targets?: Record<string, string>;
    updated_at: string;
};

export type AuthoringSkillTestResult = {
    draft: AuthoringSkillDraft;
    readiness: SkillReadiness;
    plugin_issues: string[];
    codex: CodexSkillActionResponse;
    client_readiness?: Record<string, SkillReadiness>;
    clients?: Record<string, CodexSkillActionResponse>;
};

export type ExtensionProjectKind = 'plugin' | 'skill' | 'workflow';

export type ExtensionWorkspaceSettings = {
    configured: boolean;
    valid: boolean;
    root: string;
    catalog_path: string;
    repository: string;
    default_branch: string;
    extension_count: number;
    error: string;
};

export type BuiltinAiWorkspaceTarget = { kind: 'project'; projectId: string; name: string; path: string } | { kind: 'extension-workspace'; name: string; path: string } | null;

export type ExtensionProject = {
    id: string;
    kind: ExtensionProjectKind;
    extension_id: string;
    name: string;
    description: string;
    version: string;
    workspace_path: string;
    workspace_available: boolean;
    source: string;
    source_repository: string;
    source_default_branch: string;
    source_subdirectory: string;
    source_commit: string;
    updated_at: string;
    source_unit_key?: string;
};

export type ExtensionProjectSourceInput = {
    source_repository: string;
    source_default_branch: string;
    source_subdirectory: string;
    source_commit: string;
};

export type ExtensionRemoteProject = {
    product_key: string;
    name: string;
    description: string;
    product_type: 'agent_plugin' | 'organization_skill' | 'workflow_package' | string;
    role: ExtensionCollaborationRole;
    can_manage: boolean;
    can_submit: boolean;
    source_repository: string;
    source_default_branch: string;
    source_subdirectory: string;
    updated_at: string;
};

export type CreateExtensionProjectInput = {
    kind: ExtensionProjectKind;
    slug: string;
    extension_id?: string;
    name: string;
    description: string;
    category: string;
    template?: 'readonly-tool' | 'job-worker' | 'ui-tool' | 'strict' | 'segmented' | 'flexible' | 'development-loop' | 'capability-pipeline';
};

export type ExtensionCandidate = { kind: 'plugin'; draft: AuthoringPluginDraft } | { kind: 'skill'; draft: AuthoringSkillDraft } | { kind: 'workflow'; draft: AuthoringWorkflowDraft };

export type ExtensionCollaborationRole = 'owner' | 'contributor';

export type ExtensionCollaborationMember = {
    id: string;
    product_id: string;
    product_key: string;
    user_id: string;
    user_name: string;
    role: ExtensionCollaborationRole;
    status: 'active' | 'pending' | 'declined';
    granted_by?: string;
    responded_at?: string;
    created_at: string;
    updated_at: string;
};

export type ExtensionCollaboration = {
    registered: boolean;
    product_key: string;
    product_name?: string;
    product_type?: 'agent_plugin' | 'organization_skill' | 'workflow_package' | string;
    role?: ExtensionCollaborationRole | '';
    can_manage: boolean;
    can_submit: boolean;
    source_repository?: string;
    source_default_branch?: string;
    source_subdirectory?: string;
    members: ExtensionCollaborationMember[];
};

export type ExtensionCollaboratorOption = {
    id: string;
    name: string;
    department_names: string[];
};

export type ExtensionCollaborationInvitation = {
    id: string;
    product_id: string;
    product_key: string;
    product_name: string;
    product_type: 'agent_plugin' | 'organization_skill' | 'workflow_package' | string;
    user_id: string;
    role: 'contributor';
    status: 'pending';
    invited_by: string;
    invited_by_name?: string;
    created_at: string;
    updated_at: string;
};

export type SkillSubmissionStatus = {
    id: string;
    product_key: string;
    name?: string;
    author_name?: string;
    version: string;
    status: 'submitted' | 'approved' | 'changes_requested' | 'rejected' | 'superseded';
    review_note?: string;
    release_notes?: string;
    artifact_id?: string;
    release_id?: string;
    release_status?: 'draft' | 'published' | 'revoked' | string;
    parent_release_id?: string;
    revision_of_version?: string;
    source_type?: string;
    source_repository?: string;
    source_branch?: string;
    source_subdirectory?: string;
    source_commit?: string;
    sha256?: string;
    role?: 'owner' | 'contributor';
    updated_at: string;
};

export type CodexSkillStatusResponse = {
    client_id: string;
    client_name?: string;
    client_detected?: boolean;
    skill_standard?: string;
    support_level?: 'official' | 'verified' | 'compatible' | string;
    support_note?: string;
    target_root: string;
    target_source: string;
    target_configured: boolean;
    target_kind?: SkillTargetKind;
    workspace_root?: string | null;
    workspace_id?: string | null;
    project_skills?: DiscoveredProjectSkill[];
    project_skill_conflicts?: SkillConflict[];
    target_exists: boolean;
    target_mode: 'builtin' | 'configured' | 'detected' | 'workspace' | 'preview';
    sync_mode?: 'copy' | 'symlink';
    render_mode?: 'copy' | 'symlink';
    items: CodexSkillStatusItem[];
    clients?: Record<string, CodexSkillStatusResponse>;
};

export type SkillConflict = {
    skill_id: string;
    managed_paths: string[];
    native_paths: string[];
    reason: string;
};

export type SkillSyncSettings = {
    mode: 'copy' | 'symlink';
};

export type CodexSkillSyncRendered = {
    skill_id: string;
    version: string;
    state: string;
    reason?: string | null;
    rendered_root: string;
    files: string[];
};

export type CodexSkillSyncSkipped = {
    skill_id: string;
    version: string;
    error?: string;
    state?: string;
};

export type CodexSkillSyncBlocked = {
    skill_id: string;
    version: string;
    reasons: string[];
};

export type CodexSkillSyncResponse = {
    client_id: string;
    target_root: string;
    target_source: string;
    target_configured: boolean;
    target_kind?: SkillTargetKind;
    workspace_root?: string | null;
    workspace_id?: string | null;
    rendered: CodexSkillSyncRendered[];
    skipped: CodexSkillSyncSkipped[];
    blocked: CodexSkillSyncBlocked[];
    clients?: Record<string, CodexSkillSyncResponse>;
};

export type CodexSkillUninstallResponse = {
    client_id: string;
    target_root: string;
    target_source: string;
    target_configured: boolean;
    target_kind?: SkillTargetKind;
    workspace_root?: string | null;
    workspace_id?: string | null;
    package_removed?: boolean;
    removed: {
        skill_id: string;
        removed: boolean;
    };
    clients?: Record<string, CodexSkillUninstallResponse>;
};

export type SkillClientUnregisterResponse = {
    skill_id: string;
    client_id: string;
    client_name?: string;
    target_root?: string | null;
    target_source?: string | null;
    target_configured?: boolean;
    target_kind?: SkillTargetKind;
    workspace_root?: string | null;
    workspace_id?: string | null;
    removed: {
        skill_id: string;
        removed: boolean;
    };
    state?: 'managed_elsewhere' | 'unsupported' | 'builtin' | string;
    managing_profile?: string | null;
    reason?: string | null;
};

export type SkillClientsUnregisterResponse = {
    skill_id: string;
    removed_count: number;
    results: Record<string, SkillClientUnregisterResponse>;
    failures: Record<string, string>;
};

export type CodexSkillActionResponse = {
    client_id: string;
    target_root: string;
    target_source?: string;
    target_configured?: boolean;
    target_kind?: SkillTargetKind;
    workspace_root?: string | null;
    workspace_id?: string | null;
    rendered: CodexSkillSyncRendered;
    backup_root?: string | null;
    clients?: Record<string, CodexSkillActionResponse>;
    lock_updated?: boolean;
};

export type CustomAIService = {
    id: string;
    display_name: string;
    base_url: string;
    protocol: 'openai-chat' | 'openai-responses';
    model: string;
    models: string[];
    created_at: string;
    updated_at: string;
};

export type AIProviderImportStatus = {
    target: string;
    state: string;
    client_detected: boolean;
    detail: string;
    config_path?: string;
    models?: string[];
    synced_at?: string;
    service?: string;
};

export type AIProviderImportOverview = {
    targets: AIProviderImportStatus[];
};

export type ManagedAIServiceSummary = {
    available: boolean;
    reason?: string;
    active_source?: string;
    active_entitlement_id?: string;
    active_personal_connection_id?: string;
    base_url?: string;
    model?: string;
    models?: string[];
    status?: string;
};

export type AIServiceListResult = {
    custom: { services: CustomAIService[]; active_service_id?: string };
    managed: ManagedAIServiceSummary;
    clients: AIProviderImportOverview;
};

export type AIServiceModelListResult = {
    models: string[];
};

export function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    return tauriInvoke<T>(command, args);
}

export const agentApi = {
    status: () => invoke<AgentStatus>('get_agent_status'),
    agentMode: () => invoke<AgentModeSettings>('get_agent_mode'),
    projectionSyncStatus: () => invoke<ProjectionSyncStatus>('get_projection_sync_status'),
    setAgentMode: (mode: AgentModeSettings['mode']) => invoke<AgentModeSettings>('set_agent_mode', { mode }),
    taskHistory: (limit = 50) => invoke<AgentTaskHistoryItem[]>('get_agent_task_history', { limit }),
    updateStatus: () => invoke<AgentUpdateStatus>('get_agent_update_status'),
    checkUpdate: () => invoke<AgentUpdateStatus>('check_agent_update'),
    downloadUpdate: () => invoke<AgentUpdateStatus>('download_agent_update'),
    cancelUpdateDownload: () => invoke<AgentUpdateStatus>('cancel_agent_update_download'),
    setUpdatePreferences: (autoCheck: boolean, autoDownload: boolean) => invoke<AgentUpdateStatus>('set_agent_update_preferences', { autoCheck, autoDownload }),
    installUpdate: () => invoke<AgentUpdateStatus>('install_agent_update'),
    dashboardIdentity: () => invoke<DashboardIdentityStatus>('get_dashboard_identity_status'),
    builtinAiActivity: () => invoke<{ items: BuiltinAIRuntimeActivity[] }>('get_builtin_ai_activity'),
    startDashboardAuthorization: () => invoke<DashboardAuthorizationProgress>('start_dashboard_authorization'),
    dashboardAuthorizationProgress: () => invoke<DashboardAuthorizationProgress>('get_dashboard_authorization_progress'),
    cancelDashboardAuthorization: () => invoke<DashboardAuthorizationProgress>('cancel_dashboard_authorization'),
    openDashboardAuthorizationPage: () => invoke('open_dashboard_authorization_page'),
    revokeDashboardAuthorization: () => invoke('revoke_dashboard_authorization'),
    testMcpConnection: () => invoke<McpConnectionTestResult>('test_mcp_connection'),
    mcpRegistry: () => invoke<McpRegistrySnapshot>('get_mcp_registry_snapshot'),
    mcpTargets: () => invoke<McpTargetDescriptor[]>('get_mcp_targets'),
    inspectMcpTarget: (targetId: string) => invoke<Record<string, unknown>>('inspect_mcp_target', { targetId }),
    planMcpRegistration: (targetId: string) => invoke<McpRegistrationPlan>('plan_mcp_registration', { targetId }),
    applyMcpRegistration: (targetId: string, resetInvalid = false) => invoke<McpTargetOperationResult>('apply_mcp_registration', { targetId, resetInvalid }),
    applyAllMcpRegistrations: (detectedOnly = true, resetInvalid = false) => invoke<McpTargetBatchResult>('apply_all_mcp_registrations', { detectedOnly, resetInvalid }),
    removeMcpRegistration: (targetId: string) => invoke<McpTargetOperationResult>('remove_mcp_registration', { targetId }),
    removeAllMcpRegistrations: (detectedOnly = true) => invoke<McpTargetBatchResult>('remove_all_mcp_registrations', { detectedOnly }),
    testMcpServer: (serverId: string) => invoke<McpProbeResult>('test_mcp_server', { serverId }),
    approvals: () => invoke<ApprovalItem[]>('get_pending_approvals'),
    approvalHistory: () => invoke<ApprovalFact[]>('get_approval_history'),
    settings: () => invoke<ApprovalSettings>('get_approval_settings'),
    remoteExecutionSettings: () => invoke<RemoteExecutionSettings>('get_remote_execution_settings'),
    saveRemoteExecutionSettings: (settings: RemoteExecutionSettings, fullAccessConfirmed = false) => invoke<RemoteExecutionSettings>('save_remote_execution_settings', { settings, fullAccessConfirmed }),
    remoteClients: () => invoke<RemoteClientOverview>('get_remote_clients'),
    detectRemoteClients: () => invoke<RemoteClientOverview>('detect_remote_clients'),
    configureRemoteClient: (vendor: RemoteClientVendor, path: string) => invoke<RemoteClientOverview>('configure_remote_client', { vendor, path }),
    pickRemoteClient: (vendor: RemoteClientVendor) => invoke<{ path?: string | null }>('pick_remote_client', { vendor }),
    builtinAiRuntimeStatus: () => invoke<BuiltinAIRuntimeStatus>('get_builtin_ai_runtime_status'),
    pickRuntimeManifest: () => invoke<{ path?: string | null }>('pick_runtime_manifest'),
    builtinAiRuntimeInstallationStatus: () => invoke<BuiltinAIRuntimeInstallationStatus>('get_builtin_ai_runtime_installation_status'),
    checkBuiltinAiRuntimeUpdate: () => invoke<BuiltinAIRuntimeInstallationStatus>('check_builtin_ai_runtime_update'),
    builtinAiToolContextSummary: () => invoke<BuiltinAIToolContextSummary>('get_builtin_ai_tool_context_summary'),
    builtinAiMcpServers: () => invoke<BuiltinAIMcpServer[]>('get_builtin_ai_mcp_servers'),
    saveBuiltinAiMcpServer: (server: BuiltinAIMcpServer) => invoke<BuiltinAIMcpServer>('save_builtin_ai_mcp_server', { server }),
    deleteBuiltinAiMcpServer: (serverName: string) => invoke<boolean>('delete_builtin_ai_mcp_server', { serverName }),
    validateBuiltinAiMcpServer: (server: BuiltinAIMcpServer) => invoke<void>('validate_builtin_ai_mcp_server', { server }),
    reloadBuiltinAiToolContext: () => invoke<void>('reload_builtin_ai_tool_context'),
    installBuiltinAiRuntime: () => invoke<BuiltinAIRuntimeStatus>('install_builtin_ai_runtime'),
    startBuiltinAiRuntimeInstall: (operation: BuiltinAIRuntimeInstallationStatus['operation'] = 'install', manifestPath?: string) => invoke<BuiltinAIRuntimeInstallationStatus>('start_builtin_ai_runtime_install', { operation, manifestPath }),
    startBuiltinAiSession: (target?: { projectId?: string; extensionWorkspace?: boolean }) => invoke<string>('start_builtin_ai_session', target || {}),
    openBuiltinAiWeb: (target?: { projectId?: string; extensionWorkspace?: boolean }) => invoke<string>('open_builtin_ai_web', target || {}),
    syncBuiltinAiModels: () => invoke<BuiltinAiModelSyncResult>('sync_builtin_ai_models'),
    login: () => invoke<LoginState>('get_local_login_status'),
    logs: () => invoke<LogItem[]>('get_agent_logs'),
    exportDiagnostics: () => invoke<DiagnosticsExportResult>('export_agent_diagnostics'),
    plugins: () => invoke<PluginRegistry>('get_plugin_registry'),
    workflowCenter: () => invoke<WorkflowCenterSnapshot>('get_workflow_center'),
    schedules: () => invoke<ScheduleList>('list_schedules'),
    workflowPresets: (workflowId?: string) => invoke<{ store_path: string; total: number; presets: WorkflowRunPreset[] }>('list_workflow_presets', { workflowId: workflowId ?? null }),
    setWorkflowPreset: (input: WorkflowRunPresetInput) => invoke<{ saved: boolean; preset: WorkflowRunPreset }>('set_workflow_preset', { input }),
    deleteWorkflowPreset: (id: string) => invoke<{ removed: boolean; id: string }>('delete_workflow_preset', { id }),
    setSchedule: (input: ScheduleInput) => invoke<{ saved: boolean; schedule: Schedule; store_path: string }>('set_schedule', { input }),
    deleteSchedule: (id: string) => invoke<{ removed: boolean; id: string }>('delete_schedule', { id }),
    skillRuns: (limit = 20) => invoke<SkillRunList>('list_skill_runs', { limit }),
    runSkill: (skillId: string, input: Record<string, unknown>) => invoke<{ accepted: boolean; run_id: string; run: SkillRun }>('run_skill', { skillId, input }),
    revealSkillRun: (runId: string) => invoke<void>('reveal_skill_run', { runId }),
    queryWorkflowCatalog: (q: string, category: string, page = 1, pageSize = 50) => invoke<CatalogPage<WorkflowCatalogItem>>('query_workflow_catalog', { q, category, page, pageSize }),
    workflowVersions: (workflowId: string) => invoke<WorkflowCatalogItem[]>('get_workflow_versions', { workflowId }),
    connectorStates: () => invoke<ConnectorStateItem[]>('get_connector_states'),
    setConnectorEnabled: (connectorId: string, enabled: boolean) => invoke<Record<string, unknown>>('set_connector_enabled', { connectorId, enabled }),
    revokeConnector: (connectorId: string, reason = '') => invoke<Record<string, unknown>>('revoke_connector', { connectorId, reason }),
    restoreConnector: (connectorId: string) => invoke<Record<string, unknown>>('restore_connector', { connectorId }),
    saveConnectorFileCredential: (connectorId: string, handle: string) => invoke<{ cancelled: boolean; credential: ConnectorCredentialSummary | null }>('set_connector_file_credential', { connectorId, handle }),
    saveConnectorSecretCredential: (connectorId: string, handle: string, secret: string) => invoke<ConnectorCredentialSummary>('set_connector_secret_credential', { connectorId, handle, secret }),
    removeConnectorCredential: (handle: string) => invoke<boolean>('remove_connector_credential', { handle }),
    installWorkflowCatalogItem: (workflowId: string, version?: string, source?: string, artifactId?: string, sha256?: string) => invoke<Record<string, unknown>>('install_workflow_catalog_item', { workflowId, version, source, artifactId, sha256 }),
    pickWorkflowArchive: () => invoke<{ path?: string | null }>('pick_workflow_archive'),
    installLocalWorkflowArchive: (archivePath: string, requireSignature = false) => invoke<Record<string, unknown>>('install_local_workflow_archive', { archivePath, requireSignature }),
    setWorkflowEnabled: (packageId: string, enabled: boolean) => invoke<Record<string, unknown>>('set_workflow_enabled', { packageId, enabled }),
    rollbackWorkflow: (packageId: string) => invoke<Record<string, unknown>>('rollback_workflow', { packageId }),
    removeWorkflow: (packageId: string) => invoke<boolean>('remove_workflow', { packageId }),
    workflowRun: (runId: string) => invoke<WorkflowRunSnapshot>('get_workflow_run', { runId }),
    verifyWorkflowRun: (runId: string) => invoke<WorkflowRunVerification>('verify_workflow_run', { runId }),
    revealWorkflowArtifact: (runId: string, artifactId: string) => invoke<void>('reveal_workflow_artifact', { runId, artifactId }),
    approveWorkflowStep: (runId: string, stepId: string) => invoke<WorkflowLocalRun>('approve_workflow_step', { runId, stepId }),
    rejectWorkflowStep: (runId: string, stepId: string) => invoke<WorkflowLocalRun>('reject_workflow_step', { runId, stepId }),
    cancelWorkflowRun: (runId: string) => invoke<WorkflowLocalRun>('cancel_workflow_run', { runId }),
    resumeWorkflowRun: (runId: string, feedback?: string) => invoke<{ run: WorkflowLocalRun; blocked_step_id: string; completed_steps: string[] }>('resume_workflow_run', { runId, feedback }),
    startWorkflowRun: (packageId: string, input: Record<string, unknown>) => invoke<{ run: WorkflowLocalRun; blocked_step_id: string; completed_steps: string[] }>('start_workflow_run', { packageId, input }),
    preflightWorkflowRun: (packageId: string, input: Record<string, unknown>) => invoke<WorkflowPreflight>('preflight_workflow_run', { packageId, input }),
    importLocalPlugin: () => invoke<PluginRegistry>('import_local_plugin'),
    importGithubPlugin: (sourceUrl: string) => invoke<PluginRegistry>('import_github_plugin_url', { sourceUrl }),
    extensionDesiredState: () => invoke<ExtensionDesiredState>('get_extension_desired_state'),
    extensionSources: () => invoke<ExtensionSourceSettings>('get_extension_sources'),
    addExtensionSource: (name: string, repository: string, reference: string, catalogPath?: string, verification: ExtensionSourceConfig['verification'] = 'required') =>
        invoke<ExtensionSourceSettings>('add_extension_source', { name, repository, reference, catalogPath, verification }),
    addLocalExtensionSource: (name: string, root: string, catalogPath?: string) =>
        invoke<ExtensionSourceSettings>('add_local_extension_source', { name, root, catalogPath }),
    pickLocalExtensionSourceDir: () => invoke<string | null>('pick_local_extension_source_dir'),
    updateExtensionSource: (sourceId: string, enabled: boolean, autoUpdate: boolean, verification: ExtensionSourceConfig['verification']) =>
        invoke<ExtensionSourceSettings>('update_extension_source', { sourceId, enabled, autoUpdate, verification }),
    removeExtensionSource: (sourceId: string) => invoke<ExtensionSourceSettings>('remove_extension_source', { sourceId }),
    extensionSourceSnapshot: () => invoke<ExtensionSourceSnapshot>('get_extension_source_snapshot'),
    setExtensionUnitAcquisition: (unitKey: string, acquisition: ExtensionSourceAcquisition) =>
        invoke<ExtensionSourceSettings>('set_extension_unit_acquisition', { unitKey, acquisition }),
    installExtensionUnit: (unitKey: string, sourceId: string) => invoke<ExtensionUnitInstallReport>('install_extension_unit', { unitKey, sourceId }),
    extensionProvenance: () => invoke<ExtensionProvenance[]>('get_extension_provenance'),
    pluginCatalog: () => invoke<PluginCatalogItem[]>('get_plugin_catalog'),
    queryPluginCatalog: (q: string, category: string, page = 1, pageSize = 50) => invoke<CatalogPage<PluginCatalogItem>>('query_plugin_catalog', { q, category, page, pageSize }),
    pluginDrafts: () => invoke<AuthoringPluginDraft[]>('list_plugin_drafts'),
    pluginSubmissions: () => invoke<PluginSubmissionStatus[]>('list_plugin_submissions'),
    extensionProjects: () => invoke<ExtensionProject[]>('list_extension_projects'),
    extensionWorkspace: () => invoke<ExtensionWorkspaceSettings>('get_extension_workspace'),
    setExtensionWorkspace: (root: string) => invoke<ExtensionWorkspaceSettings>('set_extension_workspace', { root }),
    extensionCollaborationProjects: () => invoke<ExtensionRemoteProject[]>('list_extension_collaboration_projects'),
    openExtensionProjects: () => invoke<ExtensionProject[]>('open_extension_projects'),
    associateExtensionProject: (project: ExtensionRemoteProject) =>
      invoke<ExtensionProject>('associate_extension_project', {
        input: {
          kind: project.product_type === 'agent_plugin'
            ? 'plugin'
            : project.product_type === 'workflow_package'
              ? 'workflow'
              : 'skill',
                extension_id: project.product_key,
                source_repository: project.source_repository,
                source_default_branch: project.source_default_branch,
                source_subdirectory: project.source_subdirectory,
                source_commit: '',
            },
        }),
    createExtensionProject: (input: CreateExtensionProjectInput) => invoke<ExtensionProject>('create_extension_project', { input }),
    buildExtensionProject: (projectId: string) => invoke<ExtensionCandidate>('build_extension_project', { projectId }),
    prepareExtensionAuthoring: () => invoke<void>('prepare_extension_authoring'),
    removeExtensionProject: (projectId: string) => invoke('remove_extension_project', { projectId }),
    updateExtensionProjectSource: (projectId: string, input: ExtensionProjectSourceInput, syncRemote = true) => invoke<ExtensionProject>('update_extension_project_source', { projectId, input, syncRemote }),
    extensionCollaboration: (productKey: string) => invoke<ExtensionCollaboration>('get_extension_collaboration', { productKey }),
    extensionCollaboratorOptions: (productKey: string, query = '') => invoke<ExtensionCollaboratorOption[]>('list_extension_collaborator_options', { productKey, query }),
    inviteExtensionCollaborator: (productKey: string, userId: string) => invoke<ExtensionCollaborationMember>('invite_extension_collaborator', { productKey, userId, role: 'contributor' }),
    deleteExtensionCollaborator: (productKey: string, userId: string) => invoke('delete_extension_collaborator', { productKey, userId }),
    extensionCollaborationInvitations: () => invoke<ExtensionCollaborationInvitation[]>('list_extension_collaboration_invitations'),
    respondExtensionCollaborationInvitation: (invitationId: string, action: 'accept' | 'decline') => invoke('respond_extension_collaboration_invitation', { invitationId, action }),
    importPluginCandidate: (revisionOfVersion?: string, parentSubmissionId?: string) => invoke<AuthoringPluginDraft>('import_plugin_candidate', { revisionOfVersion, parentSubmissionId }),
    createPluginRevision: (pluginId: string, version: string) => invoke<AuthoringPluginDraft>('create_plugin_revision', { pluginId, version }),
    testPluginDraft: (pluginId: string, version: string) => invoke<AuthoringPluginDraft>('test_plugin_draft', { pluginId, version }),
    confirmPluginDraft: (pluginId: string, version: string) => invoke<AuthoringPluginDraft>('confirm_plugin_draft', { pluginId, version }),
    workflowDrafts: () => invoke<AuthoringWorkflowDraft[]>('list_workflow_drafts'),
    testWorkflowDraft: (workflowId: string, version: string) => invoke<AuthoringWorkflowDraft>('test_workflow_draft', { workflowId, version }),
    confirmWorkflowDraft: (workflowId: string, version: string) => invoke<AuthoringWorkflowDraft>('confirm_workflow_draft', { workflowId, version }),
    submitWorkflowDraft: (workflowId: string, version: string) => invoke<AuthoringWorkflowDraft>('submit_workflow_draft', { workflowId, version }),
    workflowSubmissions: () => invoke<{ items?: AuthoringWorkflowDraft[] }>('list_workflow_submissions'),
    submitPluginDraft: (pluginId: string, version: string) => invoke<AuthoringPluginDraft>('submit_plugin_draft', { pluginId, version }),
    pluginVersions: (pluginId: string, source?: string) => invoke<PluginCatalogItem[]>('get_plugin_versions', { pluginId, source }),
    planPluginInstall: (pluginId: string, version?: string, source?: string, artifactId?: string, sha256?: string) => invoke<PluginInstallPlan>('plan_plugin_install', { pluginId, version, source, artifactId, sha256 }),
    skillCatalog: () => invoke<SkillCatalogResponse>('get_skill_catalog'),
    importLocalSkill: () => invoke<SkillImportResponse>('import_local_skill'),
    importGithubSkill: (sourceUrl: string) => invoke<SkillImportResponse>('import_github_skill_url', { sourceUrl }),
    organizationSkillCatalog: () => invoke<OrganizationSkillCatalogItem[]>('get_organization_skill_catalog'),
    queryOrganizationSkillCatalog: (q: string, category: string, page = 1, pageSize = 50) => invoke<CatalogPage<OrganizationSkillCatalogItem>>('query_organization_skill_catalog', { q, category, page, pageSize }),
    skillVersions: (skillId: string, source?: string) => invoke<OrganizationSkillCatalogItem[]>('get_skill_versions', { skillId, source }),
    planOrganizationSkillInstall: (skillId: string, version?: string, source?: string, artifactId?: string, sha256?: string) => invoke<SkillInstallPlan>('plan_organization_skill_install', { skillId, version, source, artifactId, sha256 }),
    installOrganizationSkill: (skillId: string, version?: string, optionalPluginIds: string[] = [], source?: string, artifactId?: string, sha256?: string) => invoke<OrganizationSkillInstallResponse>('install_organization_skill', { skillId, version, optionalPluginIds, source, artifactId, sha256 }),
    skillDrafts: () => invoke<AuthoringSkillDraft[]>('list_skill_drafts'),
    skillSubmissions: () => invoke<SkillSubmissionStatus[]>('list_skill_submissions'),
    importSkillCandidate: (revisionOfVersion?: string, parentSubmissionId?: string) => invoke<AuthoringSkillDraft>('import_skill_candidate', { revisionOfVersion, parentSubmissionId }),
    saveSkillDraft: (input: AuthoringSkillDraftInput) => invoke<AuthoringSkillDraft>('save_skill_draft', { input }),
    createSkillRevision: (skillId: string, version: string) => invoke<AuthoringSkillDraft>('create_skill_revision', { skillId, version }),
    testSkillDraft: (skillId: string, version: string) => invoke<AuthoringSkillTestResult>('test_skill_draft', { skillId, version }),
    confirmSkillDraft: (skillId: string, version: string) => invoke<AuthoringSkillDraft>('confirm_skill_draft', { skillId, version }),
    submitSkillDraft: (skillId: string, version: string) => invoke<AuthoringSkillDraft>('submit_skill_draft', { skillId, version }),
    codexSkillStatus: () => invoke<CodexSkillStatusResponse>('get_codex_skill_status'),
    skillWorkspace: () => invoke<SkillWorkspaceStatus>('get_skill_workspace'),
    setSkillWorkspace: (path?: string) => invoke<SkillWorkspaceStatus>('set_skill_workspace', { path: path || null }),
    setSkillWorkspaceEnabled: (skillId: string, enabled: boolean) => invoke<boolean>('set_skill_workspace_enabled', { skillId, enabled }),
    pickSkillWorkspace: () => invoke<SkillWorkspaceStatus>('pick_skill_workspace'),
    skillSyncSettings: () => invoke<SkillSyncSettings>('get_skill_sync_settings'),
    setSkillSyncMode: (mode: SkillSyncSettings['mode']) => invoke<SkillSyncSettings>('set_skill_sync_mode', { mode }),
    syncCodexSkills: () => invoke<CodexSkillSyncResponse>('sync_codex_skills'),
    syncCodexSkill: (skillId: string) => invoke<CodexSkillActionResponse>('sync_codex_skill', { skillId }),
    updateSkillWorkspace: (skillId: string) => invoke<CodexSkillActionResponse>('update_skill_workspace', { skillId }),
    syncSkillClient: (skillId: string, clientId: string) => invoke<CodexSkillActionResponse>('sync_skill_client', { skillId, clientId }),
    repairCodexSkill: (skillId: string, preserveModified = true) => invoke<CodexSkillActionResponse>('repair_codex_skill', { skillId, preserveModified }),
    uninstallCodexSkill: (skillId: string) => invoke<CodexSkillUninstallResponse>('uninstall_codex_skill', { skillId }),
    unregisterSkillClient: (skillId: string, clientId: string) => invoke<SkillClientUnregisterResponse>('unregister_skill_client', { skillId, clientId }),
    unregisterSkillClients: (skillId: string) => invoke<SkillClientsUnregisterResponse>('unregister_skill_clients', { skillId }),
    openFolder: (path: string) => invoke('open_folder', { path }),
    pickWorkspaceDirectory: () => invoke<{ path?: string }>('pick_workspace_directory'),
    installPlugin: (pluginId: string, version?: string, source?: string, artifactId?: string, sha256?: string) => invoke('install_plugin', { pluginId, version, source, artifactId, sha256 }),
    uninstallPlugin: (pluginId: string) => invoke('uninstall_plugin', { pluginId }),
    rollbackPlugin: (pluginId: string) => invoke('rollback_plugin', { pluginId }),
    setPluginEnabled: (pluginId: string, enabled: boolean) => invoke('set_plugin_enabled', { pluginId, enabled }),
    capabilities: () => invoke<CapabilityItem[]>('get_agent_capabilities'),
    fetchAIServiceModels: (input: { base_url: string; api_key: string }) => invoke<AIServiceModelListResult>('fetch_ai_service_models', { baseUrl: input.base_url, apiKey: input.api_key }),
    fetchSavedAIServiceModels: (id: string, baseUrl: string) => invoke<AIServiceModelListResult>('fetch_saved_ai_service_models', { id, baseUrl }),
    listAIServices: () => invoke<AIServiceListResult>('list_ai_services'),
    acpRuntimeProfiles: () => invoke<AcpRuntimeProfileSnapshot>('list_acp_runtime_profiles'),
    saveAcpRuntimeProfile: (input: AcpRuntimeProfileInput) => invoke<AcpRuntimeProfile>('save_acp_runtime_profile', input),
    setAcpRuntimeProfileEnabled: (providerId: string, enabled: boolean) => invoke<AcpRuntimeProfile>('set_acp_runtime_profile_enabled', { providerId, enabled }),
    removeAcpRuntimeProfile: (providerId: string) => invoke<{ provider_id: string; removed: boolean }>('remove_acp_runtime_profile', { providerId }),
    saveAIService: (input: { id: string; display_name: string; base_url: string; protocol: 'openai-chat' | 'openai-responses'; model: string; models: string[]; api_key: string }) =>
        invoke<CustomAIService>('save_ai_service', {
            id: input.id,
            displayName: input.display_name,
            baseUrl: input.base_url,
            protocol: input.protocol,
            model: input.model,
            models: input.models,
            apiKey: input.api_key,
        }),
    setActiveAIService: (id: string) => invoke<{ active_service_id: string }>('set_active_ai_service', { id }),
    removeAIService: (id: string) => invoke<boolean>('remove_ai_service', { id }),
    importAIClient: (target: string, service?: string) => invoke<Record<string, unknown>>('import_ai_client', { target, service }),
    removeAIClient: (target: string) => invoke<Record<string, unknown>>('remove_ai_client', { target }),
    respondApproval: (id: string, approved: boolean) => invoke('respond_approval', { id, approved }),
    setRule: (requestType: string, mode: string) => invoke('set_approval_rule', { requestType, mode }),
    setApprovalProfile: (profile: string, confirmed = false, durationSeconds?: number) => invoke<{ profile: string; notification_mode: string }>('set_approval_profile', { profile, confirmed, ...(durationSeconds !== undefined ? { durationSeconds } : {}) }),
    setApprovalNotificationMode: (mode: string) => invoke<{ profile: string; notification_mode: string }>('set_approval_notification_mode', { mode }),
    setTimeout: (seconds: number) => invoke('set_approval_timeout', { seconds }),
    setAutoStart: (enabled: boolean) => invoke<{ auto_start: boolean }>('set_auto_start', { enabled }),
    pickUnityEditor: () => invoke<{ path?: string }>('pick_unity_editor'),
    saveUnityEditor: (path: string) => invoke<UnityEditorSettings>('save_unity_editor', { path }),
    saveLogin: (username: string, password: string) => invoke<LoginState>('save_local_login', { username, password }),
    logoutLogin: () => invoke<LoginState>('logout_local_login'),
    svnConnections: () => invoke<{ items: SvnConnection[] }>('get_svn_connections'),
    saveSvnConnection: (request: SvnConnectionInput) => invoke<{ connection: SvnConnection }>('save_svn_connection', { request }),
    removeSvnConnection: () => invoke<{ removed: boolean }>('remove_svn_connection'),
    testSvnConnection: () => invoke<SvnConnectionTest>('test_svn_connection'),
    openDashboard: () => invoke('open_dashboard_page'),
    openInnerAdmin: () => invoke('open_inner_admin_page'),
    openAgentDirectory: () => invoke('open_agent_directory'),
    quitAgent: () => invoke('quit_agent'),
    openPluginDirectory: () => invoke('open_plugin_directory'),
    registerDevelopmentPlugin: () => invoke<string>('register_development_plugin'),
    unregisterDevelopmentPlugin: (pluginId: string) => invoke('unregister_development_plugin', { pluginId }),
    invokeDevelopmentPlugin: (pluginId: string, capabilityId: string, input: unknown) => invoke<DevelopmentInvocationResult>('invoke_development_plugin', { pluginId, capabilityId, input }),
    openPluginView: (pluginId: string, viewId: string) => invoke('open_plugin_view', { pluginId, viewId }),
    createPluginViewShortcut: (pluginId: string, viewId: string, title: string) => invoke('create_plugin_view_shortcut', { pluginId, viewId, title }),
};
