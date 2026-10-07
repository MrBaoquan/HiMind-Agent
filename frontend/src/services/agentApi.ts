import { invoke as tauriInvoke } from '@tauri-apps/api/core';
import type { CatalogView } from '../pages/mcpCatalogView';

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
    /** 同步失败按原因归组，界面只展示最主要的一类，用来判断该不该重投。 */
    dead_letter_reasons?: ProjectionDeadLetterGroup[];
};

export type ProjectionDeadLetterGroup = {
    last_error: string;
    count: number;
    oldest_at: string;
    newest_at: string;
};

/** 手工重投结果：重投把记录放回队列，紧随其后的同步由投影循环接手。 */
export type ProjectionRequeueReport = {
    requeued: number;
    dead_letter_before: number;
    dead_letter_after: number;
    pending_after: number;
    remaining_reasons: ProjectionDeadLetterGroup[];
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

/**
 * 本机运行记录（工作流 / 技能 / 定时派发）。字段刻意与任务历史对齐，
 * 让同一张列表能同时渲染工作台下发任务和本机运行，不必各自写一套行。
 */
export type AgentActivityItem = {
    id: string;
    source: 'workflow' | 'skill' | 'schedule' | string;
    title: string;
    subtitle?: string;
    status: string;
    /** 进度百分比；本机技能运行拿不到步骤指标时为 null，界面按「进行中」显示而不是 0%。 */
    progress: number | null;
    step_done?: number | null;
    step_total?: number | null;
    detail?: string | null;
    error?: string | null;
    created_at: string;
    started_at?: string | null;
    finished_at?: string | null;
    updated_at: string;
    artifact_count?: number;
    workflow_run_id?: string;
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
    unreal?: UnrealEditorSettings;
};

export type UnrealEditorSettings = {
    unreal_editor_path: string;
    environment_path: string;
    discovered_path?: string;
    source: 'agent' | 'environment' | 'discovered' | 'unset';
    valid: boolean;
};

/** 本机已安装的引擎编辑器，供「工程构建」选版本。 */
export type EngineInstallation = {
    engine: 'unity' | 'unreal';
    version: string;
    path: string;
    source: string;
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
    /**
     * 允许取值。只写字符串时值与显示文本相同；写成 `{ value, label }`
     * 时提交 `value`、显示 `label`（例如展馆 szkjg / 随州科技馆）。
     */
    options?: Array<string | { value: string; label?: string }>;
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
 * 平台级定时计划：一条计划描述“什么时候、对什么目标做什么”。
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
    /** 常用（置顶）：界面上叫「常用」，排在列表最前面。 */
    pinned?: boolean;
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
    /** 不传表示不改动置顶状态：改参数不该顺手把「常用」抹掉。 */
    pinned?: boolean;
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
    /**
     * 已安装但读取失败的制品。单个坏包不再让整份列表消失，UI 必须把它显示出来
     * 并给出移除出口，否则用户被卡在「装了什么、为什么用不了」的黑箱里。
     */
    library_issues?: WorkflowLibraryIssue[];
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

export type WorkflowLibraryIssue = {
    package_id: string;
    version: string;
    message: string;
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
    skills: Array<{ id: string; available: boolean; required?: boolean; version: string; scope: string }>;
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
      // 预设客户端需要的前置命令是否可用（npx / node / opencode），前端据此提前拦掉不可用的接入。
      // source=install_location 表示命令不在 PATH、只在已知安装位置找到（OpenCode 桌面版），
      // 这种情况要把 path 写进配置；version/config_dir 只用于展示。
      executables?: Record<string, {
        available: boolean;
        path: string;
        version?: string;
        source?: string;
        config_dir?: string;
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

export type AgentBackupCategorySummary = {
    category: string;
    files: number;
    bytes: number;
};

export type AgentBackupSkippedEntry = {
    path: string;
    reason: string;
    size: number;
};

export type AgentBackupScopeEntry = {
    name: string;
    category: string;
    reason: string;
    included: boolean;
};

export type AgentBackupScope = {
    entries: AgentBackupScopeEntry[];
    minPassphraseChars: number;
    format: string;
    formatVersion: number;
};

export type AgentBackupExportReport = {
    path: string;
    created_at: string;
    file_count: number;
    total_bytes: number;
    credentials: number;
    includes_device_identity: boolean;
    categories: AgentBackupCategorySummary[];
    skipped: AgentBackupSkippedEntry[];
    warnings: string[];
};

export type AgentBackupInspectReport = {
    path: string;
    format: string;
    version: number;
    created_at: string;
    machine: string;
    agent_version: string;
    profile: string;
    includes_device_identity: boolean;
    needs_passphrase: boolean;
    file_count: number;
    total_bytes: number;
    credentials: number;
    credential_files: string[];
    categories: AgentBackupCategorySummary[];
    skipped: AgentBackupSkippedEntry[];
    warnings: string[];
};

export type AgentBackupRestoreReport = {
    path: string;
    snapshot: string;
    restored: string[];
    credentials: number;
    credential_failures: string[];
    missing_paths: { path: string; source: string }[];
    pending_push: string[];
    warnings: string[];
};

export type AgentBackupExportResult = { canceled: true } | { canceled: false; report: AgentBackupExportReport };
export type AgentBackupInspectResult = { canceled: true } | { canceled: false; report: AgentBackupInspectReport };
export type AgentBackupRestoreResult = { canceled: true } | { canceled: false; report: AgentBackupRestoreReport };

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
    distribution_id?: string;
    channel?: string;
    catalog_id?: string;
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
    distribution_id?: string;
    channel?: string;
    catalog_id?: string;
    acquisition: ExtensionSourceAcquisition;
    local_source_id?: string | null;
    remote_source_id?: string | null;
    repository: string;
    local_root?: string | null;
    plugin_count: number;
    skill_count: number;
    workflow_count: number;
    expert_count: number;
    state: 'ready' | 'empty' | string;
    plugin_ids: string[];
    skill_ids: string[];
    workflow_ids: string[];
    expert_ids: string[];
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
    experts: ExtensionUnitAsset[];
    errors: string[];
    failures?: { asset_kind: string; asset_id: string; message: string; retryable: boolean }[];
    retryable?: boolean;
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
    expert_count: number;
    generation: string;
    using_cache: boolean;
    error: string;
    versions?: { asset_kind: string; asset_id: string; version: string }[];
    source_commit?: string;
    source_tree?: string;
    source_dirty?: boolean;
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

export type InstructionPackCatalogItem = {
  instruction_pack_id: string;
  name: string;
  description: string;
  author_name: string;
  categories: string[];
  version: string;
  release_notes: string;
  published_at: string;
  min_agent_version: string;
  supported_clients: string[];
  scope: string;
  max_bytes: number;
  channel: string;
  product_id: string;
  release_id: string;
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
  managed: boolean;
  allow_disable: boolean;
  allow_uninstall: boolean;
};

export type ExpertSummary = {
  id: string;
  name: string;
  version: string;
  description: string;
  author: string;
  categories: string[];
  supported_clients: string[];
  skill_count: number;
  workflow_count: number;
  capability_count: number;
  digest: string;
  builtin: boolean;
  active: boolean;
};

export type ExpertCatalogItem = {
  expert_id: string;
  name: string;
  description: string;
  author_name: string;
  categories: string[];
  version: string;
  release_notes: string;
  supported_clients: string[];
  artifact_id: string;
  sha256: string;
  file_size: number;
  source: string;
  assignment: string;
  management: string;
  managed: boolean;
  download_url?: string;
};

export type ExpertActivation = {
  expert_id: string;
  version: string;
  digest: string;
  activated_at: string;
};

export type ExpertPackageResult = {
  expert: ExpertSummary;
  package_path: string;
  package_sha256: string;
};

export type ExpertProjectionReceipt = {
  schema_version: string;
  expert_id: string;
  expert_version: string;
  expert_digest: string;
  client_id: string;
  workspace_root: string;
  target_path: string;
  content_digest: string;
  changed?: boolean;
  previous_digest?: string;
  backup_path?: string;
  sync_status?: string;
  verification_status?: string;
  message?: string;
  projected_at: string;
};

export type InstructionPackRef = {
  id: string;
  version: string;
  digest: string;
};

export type WorkspaceInstructionPack = {
  id: string;
  name: string;
  version: string;
  description: string;
  digest: string;
  supported_clients: string[];
  scope: string;
};

export type WorkspaceInstructionContext = {
  workspace_root: string;
  selected: InstructionPackRef[];
  available: WorkspaceInstructionPack[];
};

export type ExtensionSourceSnapshot = {
    plugins: PluginCatalogItem[];
    skills: OrganizationSkillCatalogItem[];
    workflows: WorkflowCatalogItem[];
    experts: ExpertCatalogItem[];
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

/// 批量更新的安全分组：ready 来源已核对可直接更新，review 需用户显式确认，
/// managed 由组织统一推进版本、不参与批量更新。分组由后端按安装台账计算。
export type ExtensionUpdateCandidate = {
    asset_kind: 'plugin' | 'skill' | 'workflow' | string;
    asset_id: string;
    name: string;
    installed_version: string;
    target_version: string;
    source_id: string;
    source_name: string;
    channel: string;
    sha256: string;
    artifact_id: string;
    group: 'ready' | 'review' | 'managed' | string;
    reason: string;
};

export type ExtensionUpdateTarget = {
    asset_kind: string;
    asset_id: string;
    version: string;
    source_id: string;
    sha256: string;
    artifact_id: string;
};

export type ExtensionUpdateOutcome = {
    asset_kind: string;
    asset_id: string;
    name: string;
    from_version: string;
    to_version: string;
    status: 'updated' | 'failed' | 'cancelled' | string;
    message: string;
    retryable: boolean;
};

export type ExtensionBatchUpdateReport = {
    outcomes: ExtensionUpdateOutcome[];
    updated_count: number;
    failed_count: number;
    cancelled: boolean;
};

export type ExtensionUpdateProgress = {
    index: number;
    total: number;
    asset_kind: string;
    asset_id: string;
    name: string;
    from_version: string;
    to_version: string;
    status: 'running' | 'updated' | 'failed' | 'cancelled' | string;
    message: string;
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
    /// 本机开发登记接管了同名已安装副本时，这里是被接管的已安装版本。
    overrides_installed_version?: string | null;
    failure_count?: number;
    last_failure_at?: number | null;
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

/// 一个工作台连接：地址 + 它自己的身份。本机能力只有一份，身份按连接各存一份。
export type WorkbenchConnection = {
    id: string;
    display_name: string;
    purpose: string;
    api_base: string;
    active: boolean;
    registered: boolean;
    authorized: boolean;
    agent_id: string;
    user_id: string;
    user_name: string;
    scope: string[];
    authorized_at: number;
    refresh_expires_at: number;
    last_used_at: number;
    state: 'authorized' | 'registered' | 'unregistered' | string;
};

export type WorkbenchConnectionsSnapshot = {
    api_base: string;
    connections: WorkbenchConnection[];
};

export type WorkbenchProbe = {
    api_base: string;
    reachable: boolean;
    status: number;
    message: string;
    version: string;
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

/** 工具目录的视图与安装请求，字段与后端 mcp_catalog.rs 一一对应。 */
export type McpCatalogView = CatalogView;

export type McpCatalogInstallRequest = {
    source_id: string;
    entry_id: string;
    values: Record<string, string>;
    display_name: string;
    server_name: string;
    acknowledge: boolean;
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

export type InstructionProjectionStatus =
    | 'native_loaded'
    | 'projected_managed'
    | 'projected_degraded'
    | 'conflict'
    | 'blocked'
    | string;

export type InstructionProjectionTarget = {
    adapter_id: string;
    client_id: string;
    path: string;
    scope: string;
    format: string;
    supports_global: boolean;
    supports_project: boolean;
    supports_directory: boolean;
    instruction_packs: Array<{ id: string; version: string; digest: string }>;
};

export type InstructionTargetDescriptor = {
    target: InstructionProjectionTarget;
    detected: boolean;
    native: boolean;
    degraded: boolean;
    reason: string;
    receipt?: InstructionProjectionReceipt | null;
};

export type InstructionProjectionPlan = {
    schema_version: string;
    adapter_id: string;
    client_id: string;
    target_path: string;
    snapshot_digest: string;
    expected_current_digest: string;
    status: InstructionProjectionStatus;
    writes: Array<{
        path: string;
        content_digest: string;
        content_bytes: number;
        managed_key: string;
        content?: string;
        backup_path: string;
    }>;
    conflicts: string[];
    unsupported: string[];
    warnings: string[];
    managed_block?: string;
};

export type InstructionProjectionReceipt = {
    schema_version: string;
    adapter_id: string;
    client_id: string;
    target_path: string;
    status: InstructionProjectionStatus;
    changed: boolean;
    backup_path: string;
    previous_digest: string;
    new_digest: string;
    managed_digest: string;
    managed_keys: string[];
    message: string;
};

export type EccArtifact = {
    path: string;
    kind: 'workspace_instruction' | 'subagent_template' | 'instruction_pack' | 'skill' | 'workflow' | 'hook_candidate' | 'unknown' | string;
    bytes: number;
    digest: string;
    title: string;
    frontmatter: Record<string, unknown>;
    warnings: string[];
};

export type EccInspection = {
    root: string;
    artifacts: EccArtifact[];
    warnings: string[];
    executable_files_ignored: number;
};

export type InstructionPackDraft = {
    manifest: {
        schema_version: string;
        id: string;
        name: string;
        author: string;
        categories: string[];
        version: string;
        description: string;
        release_notes: string;
        min_agent_version: string;
        supported_clients: string[];
        scope: 'global' | 'project' | 'directory' | string;
        max_bytes: number;
        skill_refs: string[];
        workflow_refs: string[];
        capability_refs: string[];
        contents: string[];
    };
    instructions: string;
    files: Record<string, string>;
    candidate_path: string;
    candidate_sha256: string;
    source_path?: string | null;
    source_sha256?: string | null;
    source: string;
    tested_at?: string | null;
    confirmed_at?: string | null;
    published_at?: string | null;
    published_digest?: string | null;
    test_report?: Record<string, unknown> | null;
    updated_at: string;
};

export type InstructionPackTestResult = {
    draft: InstructionPackDraft;
    readiness: 'ready' | 'blocked' | string;
    issues: string[];
    client_status: Record<string, string>;
};

export type InstructionPackDraftInput = {
    id: string;
    name: string;
    author?: string;
    categories?: string[];
    version: string;
    description?: string;
    release_notes: string;
    min_agent_version?: string;
    supported_clients?: string[];
    scope?: 'global' | 'project' | 'directory' | string;
    max_bytes?: number;
    instructions: string;
    files?: Record<string, string>;
    source?: string;
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
    /** global = 各 AI 工具的用户目录；directory = 用户指定的某个目录 */
    target_scope?: 'global' | 'directory';
    /** 指定目录安装时的位置根目录 */
    location_root?: string | null;
    rendered: boolean;
    rendered_valid: boolean;
    /** `render_stale` = 内容与收据一致，只是渲染方式与当前设置不同，需要重新同步。 */
    client_state: 'not_installed' | 'installed' | 'outdated' | 'modified' | 'render_stale' | 'managed_elsewhere' | 'blocked' | 'unsupported' | 'failed';
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
    /** 统一操作计划：安装 / 发布共用一份形状，界面只渲染这一种计划卡。 */
    plan?: OperationPlan;
};

export type SkillInstallPlan = {
    skill: OrganizationSkillCatalogItem;
    plugin_actions: SkillPluginInstallAction[];
    blocked_reasons: string[];
    ready: boolean;
    plan?: OperationPlan;
};

/**
 * 统一操作计划（dry-run）。
 *
 * 安装与发布在后端被收敛成同一份结构：目标（会写到哪里）、策略（怎么写）、
 * 步骤（会依次做什么）、依赖（会连带处理什么）、阻断（为什么现在不能做）。
 * 界面不再各自解释一遍"会发生什么"。
 */
export type PlanTarget = {
    /** `agent` / `client` / `github` / `workbench` / `organization` */
    kind: string;
    id: string;
    label: string;
    /** 解析后的真实落点：目录路径、`owner/repo`、目录项 ID。 */
    destination: string;
    /** `agent` / `user` / `project` / `remote` */
    scope: string;
    /** `store` / `copy` / `symlink` / `extract` / `release` / `submit` */
    strategy: string;
    /** 该落点当前是否已存在；false 表示这次操作会新建它。 */
    detected: boolean;
};

export type PlanStep = {
    id: string;
    title: string;
    detail: string;
    /** 是否会改变本机或远端状态。 */
    mutating: boolean;
};

export type PlanDependency = {
    kind: string;
    id: string;
    name: string;
    required: boolean;
    current_version: string;
    target_version: string;
    /**
     * `install` / `update` / `satisfied` / `keep` / `resolve` / `blocked` / `unavailable`
     * 文案与状态判定集中在 `components/operationPlanText.ts`，界面不直接读这个取值。
     */
    action: string;
    reason: string;
};

export type PlanItem = {
    id: string;
    name: string;
    version: string;
    description: string;
    source: string;
    artifact_id: string;
    sha256: string;
    size_bytes: number;
};

export type OperationPlan = {
    schema_version: string;
    /** `install` / `publish` */
    operation: string;
    /** `skill` / `plugin` */
    capability: string;
    item: PlanItem;
    targets: PlanTarget[];
    dependencies: PlanDependency[];
    steps: PlanStep[];
    blocked_reasons: string[];
    warnings: string[];
    ready: boolean;
};

/**
 * 客户端 × 作用域 × 能力矩阵。
 *
 * 静态能力声明跨机器一致，`availability` 是本机运行期覆盖层。功能开关以这份
 * 矩阵为准，页面不再各自维护客户端清单。
 */
export type ClientCapabilityMatrixClient = {
    id: string;
    name: string;
    aliases?: string[];
    support_level: 'official' | 'verified' | 'compatible' | string;
    support_note?: string;
    capabilities: {
        skills?: { standard: 'agentskills.io' | 'himind-store' | string; scopes: Record<string, { directory: string; env_key?: string }> };
        mcp?: { transports: string[]; target_ids?: string[]; config_format?: string; auto_configure?: boolean; server_id?: string };
        plugins?: { executor: 'agent' | 'client' | string; server_id?: string };
    };
    availability?: {
        state: 'ready' | 'not_installed' | 'not_configured' | 'unsupported' | string;
        detail?: string;
        detected?: boolean;
        configured?: boolean;
        scope?: 'agent' | 'user' | 'project' | 'none' | string;
        resolved_target?: string;
    };
};

export type ClientCapabilityMatrix = {
    schema_version: string;
    clients: ClientCapabilityMatrixClient[];
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

export type ExtensionProjectKind = 'plugin' | 'skill' | 'workflow' | 'expert' | 'instruction';

export type ExpertAuthoringDraft = {
  definition: ExpertDefinitionSnapshot;
  candidate_path: string;
  candidate_sha256: string;
  updated_at: string;
  tested_at?: string | null;
  confirmed_at?: string | null;
  submitted_at?: string | null;
  dashboard_release_id?: string | null;
  test_report?: Record<string, unknown> | null;
};

export type ExpertDefinitionSnapshot = {
  schema_version: string;
  id: string;
  name: string;
  author: string;
  categories: string[];
  version: string;
  release_notes: string;
  min_agent_version: string;
  description: string;
  supported_clients: string[];
  skill_refs: string[];
  workflow_refs: string[];
  capability_refs: string[];
  contents: string[];
  instructions: string;
  output_contract: { required_sections: string[] };
  harness: { behavior_phases: string[]; required_evidence: string[]; recovery_guidance: string[] };
};

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

/**
 * 「扩展开发」里的一行工作区。`available=false` 表示登记过但目录当前不可用，
 * 仍然返回用户才能把它移除。`has_catalog` 区分"目录里还没有聚合清单"与
 * "清单坏了"：前者只是还没有可整体分发的扩展，不是错误。
 */
export type ExtensionWorkspaceEntry = {
    root: string;
    name: string;
    available: boolean;
    has_catalog: boolean;
    valid: boolean;
    catalog_path: string;
    repository: string;
    default_branch: string;
    extension_count: number;
    error: string;
};

/** 一个正在运行的 HiMind AI 会话，按工作区各一条。 */
export type BuiltinAiSessionSnapshot = {
    workspace_root: string;
    url: string;
    focus_workspace: boolean;
    notice: string | null;
};

/** 扩展制品的分发落点：组织工作台或 GitHub Release。 */
export type DistributionTarget = 'workbench' | 'github';

/** GitHub 分发账号状态。永远不包含 token 本身。 */
export type GithubAccountStatus = {
    authorized: boolean;
    login: string;
    token_kind: string;
    /** `pat`（个人令牌）或 `app`（GitHub App 安装授权）。 */
    auth_kind: string;
    /** 已授权的 App client_id；撤销授权后仍保留，便于再次授权时预填。 */
    app_client_id: string;
    installation_id: string;
    installation_account: string;
    /** 是否已导入 App 私钥（只回布尔值，不回显密钥）。 */
    private_key_configured: boolean;
    repositories: string[];
    updated_at: string;
    store_path: string;
};

/** GitHub App 设备流授权信息：用户在浏览器里输入 user_code 完成授权。 */
export type GithubAppAuthorization = {
    device_code: string;
    user_code: string;
    verification_uri: string;
    /** 授权码已预填的授权页地址；为空时退回 verification_uri。 */
    verification_uri_complete: string;
    expires_in: number;
    interval: number;
};

/** 一次 App 安装：发布时用它换取短期安装令牌。 */
export type GithubAppInstallation = {
    id: string;
    account: string;
    account_type: string;
    repository_selection: string;
};

/** 设备流轮询结果；`authorized` 时会带上可绑定的安装列表。 */
export type GithubAppAuthorizationPoll = {
    state: 'authorized' | 'pending' | 'slow_down' | 'expired' | 'denied' | string;
    installations: GithubAppInstallation[] | null;
};

/** 分发台账条目：某个版本在某个落点上的发布结果。 */
export type DistributionStateEntry = {
    kind: string;
    id: string;
    version: string;
    target: DistributionTarget;
    status: 'pending' | 'published' | 'failed' | string;
    tag: string;
    release_id: string;
    html_url: string;
    asset_name: string;
    sha256: string;
    size_bytes: number;
    submission_id: string;
    release_reference: string;
    channel: string;
    published_at: string;
    error: string;
    attempts: number;
    updated_at: string;
};

export type DistributionPreview = {
    kind: string;
    id: string;
    version: string;
    name: string;
    targets: DistributionTarget[];
    github: {
        repository: string;
        branch: string;
        commit: string;
        tag: string;
        asset_name: string;
        manifest_name: string;
        sha256: string;
        size_bytes: number;
        authorized: boolean;
        login: string;
        /** 依赖锁定情况：总项数、已 pin 数、未 pin 的 id 列表。 */
        dependencies: { total: number; pinned: number; unpinned: string[]; blocked: boolean };
        /** 必需依赖无法 pin 时的阻断原因；非空表示不会发布到 GitHub。 */
        dependency_blocker: string | null;
        /** 制品签名状态：配置了私钥就带签名发布，否则按未签名发布。 */
        signature: { configured: boolean; key_id: string; error: string };
    };
    workbench: { distribution_id: string; channel: string; catalog_id: string };
    /** 统一操作计划：这次发布会写到哪里、依赖是否锁住、为什么被阻断。 */
    plan?: OperationPlan;
};

export type DistributionPublishReport = {
    kind: string;
    id: string;
    version: string;
    targets: DistributionTarget[];
    status: 'released' | 'partially_published' | 'failed' | string;
    outcomes: { target: string; status: string; detail?: unknown; error?: string }[];
};

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
    /** 生效的分发目标（清单声明 → 项目覆盖 → 分发单元默认 → 仅工作台）。 */
    distribution_targets?: DistributionTarget[];
    /** 扩展清单声明的分发落点，即本机设置的上限；空表示清单未声明。 */
    distribution_targets_declared?: DistributionTarget[];
    /** 生效目标的来源：`manifest` 清单声明、`project` 显式覆盖、`unit` 分发单元默认、`default` 系统默认。 */
    distribution_targets_source?: 'manifest' | 'project' | 'unit' | 'default' | string;
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
    product_type: 'agent_plugin' | 'organization_skill' | 'workflow_package' | 'agent_expert' | string;
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

export type ExtensionCandidate = { kind: 'plugin'; draft: AuthoringPluginDraft } | { kind: 'skill'; draft: AuthoringSkillDraft } | { kind: 'workflow'; draft: AuthoringWorkflowDraft } | { kind: 'expert'; draft: ExpertAuthoringDraft } | { kind: 'instruction'; draft: InstructionPackDraft };

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

export type SkillLocationEntry = {
  scope: 'global' | 'directory';
  root: string;
  /** 目录已不存在（用户删了或移走了），只能清理安装记录 */
  missing?: boolean;
  clients: string[];
  version: string;
  /** 该目录每个副本的落盘策略：`copy` / `symlink`。 */
  strategies?: string[];
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
    skill_locations?: Record<string, SkillLocationEntry[]>;
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

export type AIServiceProtocol = 'openai-chat' | 'openai-responses' | 'anthropic';

/** 本机 AI 服务预设模板：事实源是工作台 AI 服务目录，Agent 只做协议映射与缓存。 */
export type AIServiceTemplate = {
    id: string;
    name: string;
    category: string;
    description: string;
    base_url: string;
    protocol: AIServiceProtocol;
    default_model: string;
    models: string[];
};

export type AIServiceTemplateListResult = {
    /** workbench = 本次从工作台目录读取；cache = 上次成功缓存；unavailable = 暂无模板。 */
    source: 'workbench' | 'cache' | 'unavailable' | string;
    reason?: string;
    synced_at: string;
    items: AIServiceTemplate[];
};

export type CustomAIService = {
    id: string;
    display_name: string;
    base_url: string;
    protocol: AIServiceProtocol;
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

/** 用量窗口档位；今日只有日粒度单点，界面据此不出趋势图（ADR 0111）。 */
export type AiUsageRange = 'today' | '7d' | '30d';

/** 用量统计只做本机网关这一条口径（ADR 0113）：只有 Token 与调用次数，没有金额。 */
export type LocalUsageGroup = {
    key: string;
    label: string;
    requests: number;
    input_tokens: number;
    output_tokens: number;
    tokens: number;
};

export type LocalUsageDimension = 'client' | 'model' | 'service';

export type LocalUsageOverview = {
    available: boolean;
    range: string;
    date_from: string;
    date_to: string;
    has_trend: boolean;
    requests: number;
    input_tokens: number;
    output_tokens: number;
    cached_tokens: number;
    reasoning_tokens: number;
    /** 上游没返回用量：只累加调用次数，不估算 Token。 */
    usage_unreported: number;
    /** 上游是平台托管服务：以平台口径为准，本机合计已排除。 */
    platform_metered: number;
    skipped_records: number;
    labels: string[];
    daily_requests: number[];
    daily_tokens: number[];
    breakdowns: Partial<Record<LocalUsageDimension, LocalUsageGroup[]>>;
};

export type InferenceGatewayStatus = {
    running: boolean;
    url: string;
    port: number;
    /** 固定端口；端口被占用且已有绑定时不会退让，只在 last_error 里说明。 */
    preferred_port: number;
    /** 启动失败的硬原因（端口不可用等）。 */
    last_error: string;
    /** 不足以失败、但用户该知道的事实（例如临时换了端口）。 */
    notice: string;
    gateway_clients: Array<{ client: string; service: string; protocol: string; models: string[] }>;
    /** 直连注入的客户端：其用量不计入本机口径。 */
    direct_clients: string[];
};

export type InferenceGatewayStopReport = {
    stopped: boolean;
    switched: string[];
    failures: Array<{ client: string; error: string }>;
};

export function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    return tauriInvoke<T>(command, args);
}

export const agentApi = {
    status: () => invoke<AgentStatus>('get_agent_status'),
    agentMode: () => invoke<AgentModeSettings>('get_agent_mode'),
    projectionSyncStatus: () => invoke<ProjectionSyncStatus>('get_projection_sync_status'),
    requeueProjectionDeadLetters: (reason?: string) => invoke<ProjectionRequeueReport>('requeue_projection_dead_letters', { reason: reason ?? null }),
    setAgentMode: (mode: AgentModeSettings['mode']) => invoke<AgentModeSettings>('set_agent_mode', { mode }),
    taskHistory: (limit = 50) => invoke<AgentTaskHistoryItem[]>('get_agent_task_history', { limit }),
    localActivity: (limit = 60) => invoke<AgentActivityItem[]>('list_local_activity', { limit }),
    updateStatus: () => invoke<AgentUpdateStatus>('get_agent_update_status'),
    checkUpdate: () => invoke<AgentUpdateStatus>('check_agent_update'),
    downloadUpdate: () => invoke<AgentUpdateStatus>('download_agent_update'),
    cancelUpdateDownload: () => invoke<AgentUpdateStatus>('cancel_agent_update_download'),
    setUpdatePreferences: (autoCheck: boolean, autoDownload: boolean) => invoke<AgentUpdateStatus>('set_agent_update_preferences', { autoCheck, autoDownload }),
    installUpdate: () => invoke<AgentUpdateStatus>('install_agent_update'),
    dashboardIdentity: () => invoke<DashboardIdentityStatus>('get_dashboard_identity_status'),
    builtinAiActivity: () => invoke<{ items: BuiltinAIRuntimeActivity[] }>('get_builtin_ai_activity'),
    localUsageOverview: (range: AiUsageRange) => invoke<LocalUsageOverview>('get_local_usage_overview', { range }),
    inferenceGatewayStatus: () => invoke<InferenceGatewayStatus>('get_inference_gateway_status'),
    restartInferenceGateway: () => invoke<InferenceGatewayStatus>('restart_inference_gateway'),
    stopInferenceGatewayAndUnbind: () => invoke<InferenceGatewayStopReport>('stop_inference_gateway_and_unbind'),
    setProviderBindingMode: (target: string, mode: 'gateway' | 'direct', service?: string) =>
        invoke<{ ok: boolean; target: string; status: string; model?: string; config_path?: string }>('set_provider_binding_mode', { target, mode, service: service ?? null }),
    startDashboardAuthorization: () => invoke<DashboardAuthorizationProgress>('start_dashboard_authorization'),
    dashboardAuthorizationProgress: () => invoke<DashboardAuthorizationProgress>('get_dashboard_authorization_progress'),
    cancelDashboardAuthorization: () => invoke<DashboardAuthorizationProgress>('cancel_dashboard_authorization'),
    openDashboardAuthorizationPage: () => invoke('open_dashboard_authorization_page'),
    revokeDashboardAuthorization: () => invoke('revoke_dashboard_authorization'),
    workbenchConnections: () => invoke<WorkbenchConnectionsSnapshot>('list_workbench_connections'),
    addWorkbenchConnection: (apiBase: string, displayName: string, purpose: string) =>
        invoke<WorkbenchConnectionsSnapshot>('add_workbench_connection', { apiBase, displayName, purpose }),
    renameWorkbenchConnection: (id: string, displayName: string, purpose: string) =>
        invoke<WorkbenchConnectionsSnapshot>('rename_workbench_connection', { id, displayName, purpose }),
    removeWorkbenchConnection: (id: string) =>
        invoke<WorkbenchConnectionsSnapshot>('remove_workbench_connection', { id }),
    probeWorkbenchConnection: (apiBase: string) =>
        invoke<WorkbenchProbe>('probe_workbench_connection', { apiBase }),
    switchWorkbenchConnection: (id: string, force = false) =>
        invoke<WorkbenchConnectionsSnapshot>('switch_workbench_connection', { id, force }),
    enrollWorkbenchConnection: (id: string, enrollmentToken: string) =>
        invoke<WorkbenchConnectionsSnapshot>('enroll_workbench_connection', { id, enrollmentToken }),
    testMcpConnection: () => invoke<McpConnectionTestResult>('test_mcp_connection'),
    mcpRegistry: () => invoke<McpRegistrySnapshot>('get_mcp_registry_snapshot'),
    mcpTargets: () => invoke<McpTargetDescriptor[]>('get_mcp_targets'),
    experts: () => invoke<ExpertSummary[]>('list_experts'),
    expertDrafts: () => invoke<ExpertAuthoringDraft[]>('list_expert_drafts'),
    testExpertDraft: (expertId: string, version: string) => invoke<ExpertAuthoringDraft>('test_expert_draft', { expertId, version }),
    confirmExpertDraft: (expertId: string, version: string) => invoke<ExpertAuthoringDraft>('confirm_expert_draft', { expertId, version }),
    submitExpertDraft: (expertId: string, version: string) => invoke<ExpertAuthoringDraft>('submit_expert_draft', { expertId, version }),
    expertCatalog: () => invoke<ExpertCatalogItem[]>('get_expert_catalog'),
    activeExpert: (workspaceRoot?: string) => invoke<ExpertActivation | null>('active_expert', { workspaceRoot: workspaceRoot ?? null }),
    activateExpert: (expertId: string, version?: string, workspaceRoot?: string) => invoke<ExpertActivation>('activate_expert', { expertId, version: version ?? null, workspaceRoot: workspaceRoot ?? null }),
    saveExpert: (input: Record<string, unknown>) => invoke<ExpertSummary>('save_expert', { input }),
    pickExpertPackage: () => invoke<string | null>('pick_expert_package'),
    importExpertPackage: (path: string) => invoke<ExpertSummary>('import_expert_package', { path }),
    exportExpertPackage: (expertId: string, version: string) => invoke<ExpertPackageResult>('export_expert_package', { expertId, version }),
    projectExpertToClient: (expertId: string, clientId: string, workspaceRoot: string, version?: string) => invoke<ExpertProjectionReceipt>('project_expert_to_client', { expertId, clientId, workspaceRoot, version: version ?? null }),
    materializeExpertProject: (workspaceRoot: string, expertId: string, version?: string) => invoke<ExtensionProject>('materialize_expert_project', { workspaceRoot, expertId, version: version ?? null }),
    materializeInstructionProject: (workspaceRoot: string, instructionPackId: string, version?: string) => invoke<ExtensionProject>('materialize_instruction_project', { workspaceRoot, instructionPackId, version: version ?? null }),
    instructionTargets: (workspaceRoot: string) => invoke<InstructionTargetDescriptor[]>('get_instruction_targets', { workspaceRoot }),
    workspaceInstructionContext: (workspaceRoot: string) => invoke<WorkspaceInstructionContext>('get_workspace_instruction_context', { workspaceRoot }),
    saveWorkspaceInstructionSelection: (workspaceRoot: string, selected: InstructionPackRef[]) =>
      invoke<WorkspaceInstructionContext>('save_workspace_instruction_selection', { workspaceRoot, selected }),
    inspectEccRepository: (root: string) => invoke<EccInspection>('inspect_ecc_repository', { root }),
    pickInstructionFile: () => invoke<string | null>('pick_instruction_file'),
    pickInstructionPackage: () => invoke<string | null>('pick_instruction_package'),
    instructionPackDrafts: () => invoke<InstructionPackDraft[]>('list_instruction_pack_drafts'),
    saveInstructionPackDraft: (input: InstructionPackDraftInput) => invoke<InstructionPackDraft>('save_instruction_pack_draft', { input }),
    importInstructionFile: (path: string) => invoke<InstructionPackDraft>('import_instruction_file', { path }),
    importInstructionPackage: (path: string) => invoke<InstructionPackDraft>('import_instruction_package', { path }),
    testInstructionPackDraft: (id: string, version: string) => invoke<InstructionPackTestResult>('test_instruction_pack_draft', { id, version }),
    confirmInstructionPackDraft: (id: string, version: string) => invoke<InstructionPackDraft>('confirm_instruction_pack_draft', { id, version }),
    publishInstructionPackLocally: (id: string, version: string) => invoke<InstructionPackDraft>('publish_instruction_pack_locally', { id, version }),
    planInstructionProjection: (workspaceRoot: string, target: InstructionProjectionTarget) =>
        invoke<InstructionProjectionPlan>('plan_instruction_projection', { workspaceRoot, target }),
    applyInstructionProjection: (plan: InstructionProjectionPlan) =>
        invoke<InstructionProjectionReceipt>('apply_instruction_projection', { plan }),
    rollbackInstructionProjection: (receipt: InstructionProjectionReceipt) =>
        invoke<void>('rollback_instruction_projection', { receipt }),
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
    /** 预设要用的前置命令在不在（npx / node），用于在添加之前就把环境问题说清楚。 */
    mcpRuntimeRequirements: () => invoke<Record<string, { available: boolean; path: string }>>('get_mcp_runtime_requirements'),
    /** 工具目录（server.json）：只读本地快照，不联网。 */
    mcpCatalog: () => invoke<McpCatalogView>('get_mcp_catalog'),
    /** 拉一次目录来源。慢，但要给用户一个显式的「刷新」。 */
    refreshMcpCatalog: () => invoke<McpCatalogView>('refresh_mcp_catalog'),
    /** 从目录装一条：写入的仍然是 himind-ai-mcp.json，没有第二个存储。 */
    installMcpCatalogEntry: (request: McpCatalogInstallRequest) => invoke<BuiltinAIMcpServer>('install_mcp_catalog_entry', { request }),
    reloadBuiltinAiToolContext: () => invoke<void>('reload_builtin_ai_tool_context'),
    installBuiltinAiRuntime: () => invoke<BuiltinAIRuntimeStatus>('install_builtin_ai_runtime'),
    startBuiltinAiRuntimeInstall: (operation: BuiltinAIRuntimeInstallationStatus['operation'] = 'install', manifestPath?: string) => invoke<BuiltinAIRuntimeInstallationStatus>('start_builtin_ai_runtime_install', { operation, manifestPath }),
    /** 每个工作区各一条会话，重复调用同一个工作区只会复用已有会话。 */
    startBuiltinAiSession: (target?: { projectId?: string; workspaceRoot?: string }) => invoke<string>('start_builtin_ai_session', target || {}),
    builtinAiSessionNotice: (workspaceRoot?: string) => invoke<string | null>('get_builtin_ai_session_notice', { workspaceRoot: workspaceRoot ?? null }),
    listBuiltinAiSessions: () => invoke<BuiltinAiSessionSnapshot[]>('list_builtin_ai_sessions'),
    stopBuiltinAiSession: (workspaceRoot: string) => invoke<boolean>('stop_builtin_ai_session', { workspaceRoot }),
    openBuiltinAiWeb: (target?: { projectId?: string; workspaceRoot?: string }) => invoke<string>('open_builtin_ai_web', target || {}),
    syncBuiltinAiModels: () => invoke<BuiltinAiModelSyncResult>('sync_builtin_ai_models'),
    login: () => invoke<LoginState>('get_local_login_status'),
    logs: () => invoke<LogItem[]>('get_agent_logs'),
    exportDiagnostics: () => invoke<DiagnosticsExportResult>('export_agent_diagnostics'),
    backupScope: () => invoke<AgentBackupScope>('get_agent_backup_scope'),
    /**
     * 导出配置层备份包。`passphrase` 为空表示包里没有需要保护的凭据；
     * 一旦有账号凭据，后端会拒绝无口令导出。
     */
    exportBackup: (passphrase: string | null, includeDeviceIdentity = false) => invoke<AgentBackupExportResult>('export_agent_backup', { passphrase, includeDeviceIdentity }),
    /** 不传 `path` 时弹出文件选择框。只读检视，不写磁盘。 */
    inspectBackup: (path?: string | null) => invoke<AgentBackupInspectResult>('inspect_agent_backup', { path: path ?? null }),
    /** 恢复会先做完整校验，并在覆盖前写自动快照。 */
    importBackup: (path: string | null, passphrase: string | null) => invoke<AgentBackupRestoreResult>('import_agent_backup', { path, passphrase }),
    plugins: () => invoke<PluginRegistry>('get_plugin_registry'),
    /**
     * `light` 跳过控制面工作流目录的远程拉取，只取本机已安装工作流与运行记录；
     * 轮询必须走轻量快照，完整快照留给打开发工作流页面和手动刷新。
     */
    workflowCenter: (light = false) => invoke<WorkflowCenterSnapshot>('get_workflow_center', { light }),
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
    instructionPackCatalog: () => invoke<InstructionPackCatalogItem[]>('get_instruction_pack_catalog'),
    instructionPackVersions: (instructionPackId: string) => invoke<InstructionPackCatalogItem[]>('get_instruction_pack_versions', { instructionPackId }),
    installInstructionPackMarket: (instructionPackId: string, version?: string, artifactId?: string, sha256?: string) => invoke<Record<string, unknown>>('install_instruction_pack_market', { instructionPackId, version, artifactId, sha256 }),
    installExpertMarket: (expertId: string, version?: string, artifactId?: string, sha256?: string) => invoke<Record<string, unknown>>('install_expert_market', { expertId, version, artifactId, sha256 }),
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
    planExtensionUpdates: () => invoke<ExtensionUpdateCandidate[]>('plan_extension_updates'),
    applyExtensionUpdates: (targets: ExtensionUpdateTarget[]) => invoke<ExtensionBatchUpdateReport>('apply_extension_updates', { targets }),
    cancelExtensionUpdates: () => invoke<void>('cancel_extension_updates'),
    pluginCatalog: () => invoke<PluginCatalogItem[]>('get_plugin_catalog'),
    queryPluginCatalog: (q: string, category: string, page = 1, pageSize = 50) => invoke<CatalogPage<PluginCatalogItem>>('query_plugin_catalog', { q, category, page, pageSize }),
    pluginDrafts: () => invoke<AuthoringPluginDraft[]>('list_plugin_drafts'),
    pluginSubmissions: () => invoke<PluginSubmissionStatus[]>('list_plugin_submissions'),
    extensionProjects: () => invoke<ExtensionProject[]>('list_extension_projects'),
    extensionWorkspace: () => invoke<ExtensionWorkspaceSettings>('get_extension_workspace'),
    setExtensionWorkspace: (root: string) => invoke<ExtensionWorkspaceSettings>('set_extension_workspace', { root }),
    extensionWorkspaces: () => invoke<ExtensionWorkspaceEntry[]>('list_extension_workspaces'),
    pickExtensionWorkspaceDir: () => invoke<string | null>('pick_extension_workspace_dir'),
    addExtensionWorkspace: (root: string) => invoke<ExtensionWorkspaceEntry[]>('add_extension_workspace', { root }),
    removeExtensionWorkspace: (root: string) => invoke<ExtensionWorkspaceEntry[]>('remove_extension_workspace', { root }),
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
    createExtensionProject: (input: CreateExtensionProjectInput, parentDir?: string) => invoke<ExtensionProject>('create_extension_project', { input, parentDir: parentDir ?? null }),
    buildExtensionProject: (projectId: string) => invoke<ExtensionCandidate>('build_extension_project', { projectId }),
    /** 设置项目级分发目标；`targets = null` 表示回到分发单元默认。 */
    setExtensionProjectDistributionTargets: (kind: ExtensionProjectKind, extensionId: string, targets: DistributionTarget[] | null) =>
      invoke<ExtensionProject>('set_extension_project_distribution_targets', { kind, extensionId, targets }),
    // targets 传 null 表示回到继承：按清单声明或出厂默认。
    setExtensionUnitDistributionTargets: (unitKey: string, targets: DistributionTarget[] | null) =>
      invoke<ExtensionSourceSettings>('set_extension_unit_distribution_targets', { unitKey, targets }),
    /** 分发预览：无副作用，用于 UI 说明这次会发到哪里。 */
    previewExtensionDistribution: (kind: ExtensionProjectKind, extensionId: string, version: string) =>
      invoke<DistributionPreview>('preview_extension_distribution', { kind, extensionId, version }),
    publishExtensionDistribution: (kind: ExtensionProjectKind, extensionId: string, version: string) =>
      invoke<DistributionPublishReport>('publish_extension_distribution', { kind, extensionId, version }),
    extensionDistributionState: (kind?: ExtensionProjectKind, extensionId?: string) =>
      invoke<DistributionStateEntry[]>('get_extension_distribution_state', { kind, extensionId }),
    githubDistributionAccount: () => invoke<GithubAccountStatus>('get_github_distribution_account'),
    setGithubDistributionAccount: (token: string, tokenKind?: string, repositories?: string[]) =>
      invoke<GithubAccountStatus>('set_github_distribution_account', { token, tokenKind, repositories }),
    removeGithubDistributionAccount: () => invoke<boolean>('remove_github_distribution_account'),
    /** 设备流第一步：申请 user_code。client_id 留空时由后端回退到已保存值或环境变量。 */
    startGithubAppAuthorization: (clientId?: string) =>
      invoke<{ client_id: string; authorization: GithubAppAuthorization }>('start_github_app_authorization', { clientId }),
    pollGithubAppAuthorization: (clientId: string, deviceCode: string) =>
      invoke<GithubAppAuthorizationPoll>('poll_github_app_authorization', { clientId, deviceCode }),
    listGithubAppInstallations: () =>
      invoke<{ installations: GithubAppInstallation[] }>('list_github_app_installations'),
    selectGithubAppInstallation: (installationId: string) =>
      invoke<GithubAccountStatus>('select_github_app_installation', { installationId }),
    importGithubAppPrivateKey: () => invoke<GithubAccountStatus>('import_github_app_private_key'),
    openGithubAuthorizationPage: (verificationUri: string) =>
      invoke<void>('open_github_authorization_page', { verificationUri }),
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
    installOrganizationSkill: (skillId: string, version?: string, optionalPluginIds: string[] = [], source?: string, artifactId?: string, sha256?: string, clients?: string[], location?: string) => invoke<OrganizationSkillInstallResponse>('install_organization_skill', { skillId, version, optionalPluginIds, source, artifactId, sha256, clients, location }),
    pickSkillLocation: () => invoke<string>('pick_skill_location'),
    deploySkillToLocation: (skillId: string, location: string, clients?: string[]) => invoke<{ clients: Record<string, unknown> }>('deploy_skill_to_location', { skillId, location, clients }),
    removeSkillFromLocation: (skillId: string, location: string) => invoke<{ removed_count: number }>('remove_skill_from_location', { skillId, location }),
    skillDrafts: () => invoke<AuthoringSkillDraft[]>('list_skill_drafts'),
    skillSubmissions: () => invoke<SkillSubmissionStatus[]>('list_skill_submissions'),
    importSkillCandidate: (revisionOfVersion?: string, parentSubmissionId?: string) => invoke<AuthoringSkillDraft>('import_skill_candidate', { revisionOfVersion, parentSubmissionId }),
    saveSkillDraft: (input: AuthoringSkillDraftInput) => invoke<AuthoringSkillDraft>('save_skill_draft', { input }),
    createSkillRevision: (skillId: string, version: string) => invoke<AuthoringSkillDraft>('create_skill_revision', { skillId, version }),
    testSkillDraft: (skillId: string, version: string) => invoke<AuthoringSkillTestResult>('test_skill_draft', { skillId, version }),
    confirmSkillDraft: (skillId: string, version: string) => invoke<AuthoringSkillDraft>('confirm_skill_draft', { skillId, version }),
    submitSkillDraft: (skillId: string, version: string) => invoke<AuthoringSkillDraft>('submit_skill_draft', { skillId, version }),
    codexSkillStatus: () => invoke<CodexSkillStatusResponse>('get_codex_skill_status'),
    /** 客户端 × 作用域 × 能力矩阵：功能开关的唯一答案来源。 */
    clientCapabilityMatrix: () => invoke<ClientCapabilityMatrix>('get_client_capability_matrix'),
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
    /// 设置窗口的定位由「面板 + 条目 + 页签」三段组成：条目不适用时传空，
    /// 旧键位（remote / skills / logs……）后端原样透传，由前端统一映射。
    openSettingsWindow: (panel: 'settings' | 'ai' | 'logs' = 'settings', section?: string, tab?: string, aiTab?: 'mcp' | 'services' | 'acp') => invoke('open_settings_window', { panel, section, tab, aiTab }),
    pickWorkspaceDirectory: () => invoke<{ path?: string }>('pick_workspace_directory'),
    installPlugin: (pluginId: string, version?: string, source?: string, artifactId?: string, sha256?: string) => invoke('install_plugin', { pluginId, version, source, artifactId, sha256 }),
    uninstallPlugin: (pluginId: string) => invoke('uninstall_plugin', { pluginId }),
    rollbackPlugin: (pluginId: string) => invoke('rollback_plugin', { pluginId }),
    repairPlugin: (pluginId: string) => invoke('repair_plugin', { pluginId }),
    setPluginEnabled: (pluginId: string, enabled: boolean) => invoke('set_plugin_enabled', { pluginId, enabled }),
    capabilities: () => invoke<CapabilityItem[]>('get_agent_capabilities'),
    fetchAIServiceModels: (input: { base_url: string; api_key: string; protocol: AIServiceProtocol }) => invoke<AIServiceModelListResult>('fetch_ai_service_models', { baseUrl: input.base_url, apiKey: input.api_key, protocol: input.protocol }),
    fetchSavedAIServiceModels: (id: string, baseUrl: string) => invoke<AIServiceModelListResult>('fetch_saved_ai_service_models', { id, baseUrl }),
    listAIServices: () => invoke<AIServiceListResult>('list_ai_services'),
    listAIServiceTemplates: () => invoke<AIServiceTemplateListResult>('list_ai_service_templates'),
    acpRuntimeProfiles: () => invoke<AcpRuntimeProfileSnapshot>('list_acp_runtime_profiles'),
    saveAcpRuntimeProfile: (input: AcpRuntimeProfileInput) => invoke<AcpRuntimeProfile>('save_acp_runtime_profile', input),
    setAcpRuntimeProfileEnabled: (providerId: string, enabled: boolean) => invoke<AcpRuntimeProfile>('set_acp_runtime_profile_enabled', { providerId, enabled }),
    removeAcpRuntimeProfile: (providerId: string) => invoke<{ provider_id: string; removed: boolean }>('remove_acp_runtime_profile', { providerId }),
    saveAIService: (input: { id: string; display_name: string; base_url: string; protocol: AIServiceProtocol; model: string; models: string[]; api_key: string }) =>
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
    importAIClient: (target: string, service?: string, replace = false) => invoke<Record<string, unknown>>('import_ai_client', { target, service, replace }),
    removeAIClient: (target: string) => invoke<Record<string, unknown>>('remove_ai_client', { target }),
    respondApproval: (id: string, approved: boolean) => invoke('respond_approval', { id, approved }),
    setRule: (requestType: string, mode: string) => invoke('set_approval_rule', { requestType, mode }),
    setApprovalProfile: (profile: string, confirmed = false, durationSeconds?: number) => invoke<{ profile: string; notification_mode: string }>('set_approval_profile', { profile, confirmed, ...(durationSeconds !== undefined ? { durationSeconds } : {}) }),
    setApprovalNotificationMode: (mode: string) => invoke<{ profile: string; notification_mode: string }>('set_approval_notification_mode', { mode }),
    setTimeout: (seconds: number) => invoke('set_approval_timeout', { seconds }),
    setAutoStart: (enabled: boolean) => invoke<{ auto_start: boolean }>('set_auto_start', { enabled }),
    pickUnityEditor: () => invoke<{ path?: string }>('pick_unity_editor'),
    saveUnityEditor: (path: string) => invoke<UnityEditorSettings>('save_unity_editor', { path }),
    pickEngineEditor: (engine: 'unity' | 'unreal') => invoke<{ path?: string }>('pick_engine_editor', { engine }),
    saveEngineEditor: (engine: 'unity' | 'unreal', path: string) => invoke<UnityEditorSettings>('save_engine_editor', { engine, path }),
    engineInstallations: () => invoke<EngineInstallation[]>('list_engine_installations'),
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
