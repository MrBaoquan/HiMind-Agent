import { invoke as tauriInvoke } from '@tauri-apps/api/core';

export type AgentStatus = {
    version: string;
    dashboard_base?: string;
    dashboard_worker_online: boolean;
    dashboard_worker_error?: string;
    dashboard_agent_id?: string;
    local_port?: number;
    mode?: string;
    effective_mode?: string;
    pending_mode?: string;
    requires_restart?: boolean;
    dashboard_enabled?: boolean;
    control_plane?: {
        kind: 'none' | 'dashboard' | string;
        enabled: boolean;
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
    editors?: UnityEditorSettings;
};

export type RemoteExecutionSettings = {
    enabled: boolean;
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

export type BuiltinAIRuntimeStatus = {
    provider: 'himind.builtin' | string;
    status: 'ready' | 'unavailable' | string;
    version: string;
    compatible: boolean;
    message: string;
    diagnostics: {
        engine_id: string;
        executable_path: string;
        contract_version: number;
        update_mode: string;
    };
};

export type BuiltinAIRuntimeInstallationStatus = {
    state: 'idle' | 'working' | 'ready' | 'failed' | string;
    operation: 'none' | 'install' | 'update' | 'repair' | 'uninstall' | string;
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
    repository: string;
    reference: string;
    catalog_path: string;
    enabled: boolean;
    auto_update: boolean;
    verification: 'required' | 'optional';
};

export type ExtensionSourceSettings = {
    schema_version: number;
    sources: ExtensionSourceConfig[];
};

export type ExtensionSourceStatus = {
    source: ExtensionSourceConfig;
    state: 'ready' | 'unavailable' | string;
    plugin_count: number;
    skill_count: number;
    generation: string;
    using_cache: boolean;
    error: string;
};

export type ExtensionFeaturePack = {
    id: string;
    name: string;
    plugin_ids: string[];
    skill_ids: string[];
};

export type ExtensionSourceSnapshot = {
    plugins: PluginCatalogItem[];
    skills: OrganizationSkillCatalogItem[];
    feature_packs: ExtensionFeaturePack[];
    sources: ExtensionSourceStatus[];
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
    location?: string;
    entry: string;
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

export type ExtensionProjectKind = 'plugin' | 'skill';

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

export type BuiltinAiWorkspaceTarget =
  | { kind: 'project'; projectId: string; name: string; path: string }
  | { kind: 'extension-workspace'; name: string; path: string }
  | null;

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
    product_type: 'agent_plugin' | 'organization_skill' | string;
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
    template?: 'readonly-tool' | 'job-worker' | 'ui-tool';
};

export type ExtensionCandidate =
    | { kind: 'plugin'; draft: AuthoringPluginDraft }
    | { kind: 'skill'; draft: AuthoringSkillDraft };

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
    product_type?: 'agent_plugin' | 'organization_skill' | string;
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
    product_type: 'agent_plugin' | 'organization_skill' | string;
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
    target_exists: boolean;
  target_mode: 'builtin' | 'configured' | 'detected' | 'preview';
  sync_mode?: 'copy' | 'symlink';
    items: CodexSkillStatusItem[];
    clients?: Record<string, CodexSkillStatusResponse>;
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
    rendered: CodexSkillSyncRendered;
    backup_root?: string | null;
    clients?: Record<string, CodexSkillActionResponse>;
};

export function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    return tauriInvoke<T>(command, args);
}

export const agentApi = {
    status: () => invoke<AgentStatus>('get_agent_status'),
    agentMode: () => invoke<AgentModeSettings>('get_agent_mode'),
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
    settings: () => invoke<ApprovalSettings>('get_approval_settings'),
    remoteExecutionSettings: () => invoke<RemoteExecutionSettings>('get_remote_execution_settings'),
    saveRemoteExecutionSettings: (settings: RemoteExecutionSettings, fullAccessConfirmed = false) => invoke<RemoteExecutionSettings>('save_remote_execution_settings', { settings, fullAccessConfirmed }),
    remoteClients: () => invoke<RemoteClientOverview>('get_remote_clients'),
    detectRemoteClients: () => invoke<RemoteClientOverview>('detect_remote_clients'),
    configureRemoteClient: (vendor: RemoteClientVendor, path: string) => invoke<RemoteClientOverview>('configure_remote_client', { vendor, path }),
    pickRemoteClient: (vendor: RemoteClientVendor) => invoke<{ path?: string | null }>('pick_remote_client', { vendor }),
    builtinAiRuntimeStatus: () => invoke<BuiltinAIRuntimeStatus>('get_builtin_ai_runtime_status'),
    builtinAiRuntimeInstallationStatus: () => invoke<BuiltinAIRuntimeInstallationStatus>('get_builtin_ai_runtime_installation_status'),
    checkBuiltinAiRuntimeUpdate: () => invoke<BuiltinAIRuntimeInstallationStatus>('check_builtin_ai_runtime_update'),
    builtinAiToolContextSummary: () => invoke<BuiltinAIToolContextSummary>('get_builtin_ai_tool_context_summary'),
    builtinAiMcpServers: () => invoke<BuiltinAIMcpServer[]>('get_builtin_ai_mcp_servers'),
    saveBuiltinAiMcpServer: (server: BuiltinAIMcpServer) => invoke<BuiltinAIMcpServer>('save_builtin_ai_mcp_server', { server }),
    deleteBuiltinAiMcpServer: (serverName: string) => invoke<boolean>('delete_builtin_ai_mcp_server', { serverName }),
    validateBuiltinAiMcpServer: (server: BuiltinAIMcpServer) => invoke<void>('validate_builtin_ai_mcp_server', { server }),
    reloadBuiltinAiToolContext: () => invoke<void>('reload_builtin_ai_tool_context'),
    installBuiltinAiRuntime: () => invoke<BuiltinAIRuntimeStatus>('install_builtin_ai_runtime'),
    startBuiltinAiRuntimeInstall: (operation: BuiltinAIRuntimeInstallationStatus['operation'] = 'install') => invoke<BuiltinAIRuntimeInstallationStatus>('start_builtin_ai_runtime_install', { operation }),
    startBuiltinAiSession: (target?: { projectId?: string; extensionWorkspace?: boolean }) => invoke<string>('start_builtin_ai_session', target || {}),
    syncBuiltinAiModels: () => invoke<BuiltinAiModelSyncResult>('sync_builtin_ai_models'),
    login: () => invoke<LoginState>('get_local_login_status'),
    logs: () => invoke<LogItem[]>('get_agent_logs'),
    exportDiagnostics: () => invoke<DiagnosticsExportResult>('export_agent_diagnostics'),
    plugins: () => invoke<PluginRegistry>('get_plugin_registry'),
    importLocalPlugin: () => invoke<PluginRegistry>('import_local_plugin'),
    importGithubPlugin: (sourceUrl: string) => invoke<PluginRegistry>('import_github_plugin_url', { sourceUrl }),
    extensionDesiredState: () => invoke<ExtensionDesiredState>('get_extension_desired_state'),
    extensionSources: () => invoke<ExtensionSourceSettings>('get_extension_sources'),
    addExtensionSource: (name: string, repository: string, reference: string, catalogPath?: string, verification: ExtensionSourceConfig['verification'] = 'required') => invoke<ExtensionSourceSettings>('add_extension_source', { name, repository, reference, catalogPath, verification }),
    updateExtensionSource: (sourceId: string, enabled: boolean, autoUpdate: boolean, verification: ExtensionSourceConfig['verification']) => invoke<ExtensionSourceSettings>('update_extension_source', { sourceId, enabled, autoUpdate, verification }),
    removeExtensionSource: (sourceId: string) => invoke<ExtensionSourceSettings>('remove_extension_source', { sourceId }),
    extensionSourceSnapshot: () => invoke<ExtensionSourceSnapshot>('get_extension_source_snapshot'),
    extensionProvenance: () => invoke<ExtensionProvenance[]>('get_extension_provenance'),
    pluginCatalog: () => invoke<PluginCatalogItem[]>('get_plugin_catalog'),
    queryPluginCatalog: (q: string, category: string, page = 1, pageSize = 50) => invoke<CatalogPage<PluginCatalogItem>>('query_plugin_catalog', { q, category, page, pageSize }),
    pluginDrafts: () => invoke<AuthoringPluginDraft[]>('list_plugin_drafts'),
    pluginSubmissions: () => invoke<PluginSubmissionStatus[]>('list_plugin_submissions'),
    extensionProjects: () => invoke<ExtensionProject[]>('list_extension_projects'),
    extensionWorkspace: () => invoke<ExtensionWorkspaceSettings>('get_extension_workspace'),
    selectExtensionWorkspace: () => invoke<ExtensionWorkspaceSettings>('select_extension_workspace'),
    extensionCollaborationProjects: () => invoke<ExtensionRemoteProject[]>('list_extension_collaboration_projects'),
    openExtensionProjects: () => invoke<ExtensionProject[]>('open_extension_projects'),
    associateExtensionProject: (project: ExtensionRemoteProject) => invoke<ExtensionProject>('associate_extension_project', { input: { kind: project.product_type === 'agent_plugin' ? 'plugin' : 'skill', extension_id: project.product_key, source_repository: project.source_repository, source_default_branch: project.source_default_branch, source_subdirectory: project.source_subdirectory, source_commit: '' } }),
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
    submitPluginDraft: (pluginId: string, version: string) => invoke<AuthoringPluginDraft>('submit_plugin_draft', { pluginId, version }),
    pluginVersions: (pluginId: string) => invoke<PluginCatalogItem[]>('get_plugin_versions', { pluginId }),
    planPluginInstall: (pluginId: string, version?: string) => invoke<PluginInstallPlan>('plan_plugin_install', { pluginId, version }),
    skillCatalog: () => invoke<SkillCatalogResponse>('get_skill_catalog'),
    importLocalSkill: () => invoke<SkillRecord>('import_local_skill'),
    importGithubSkill: (sourceUrl: string) => invoke<SkillRecord>('import_github_skill_url', { sourceUrl }),
    organizationSkillCatalog: () => invoke<OrganizationSkillCatalogItem[]>('get_organization_skill_catalog'),
    queryOrganizationSkillCatalog: (q: string, category: string, page = 1, pageSize = 50) => invoke<CatalogPage<OrganizationSkillCatalogItem>>('query_organization_skill_catalog', { q, category, page, pageSize }),
    skillVersions: (skillId: string) => invoke<OrganizationSkillCatalogItem[]>('get_skill_versions', { skillId }),
    planOrganizationSkillInstall: (skillId: string, version?: string) => invoke<SkillInstallPlan>('plan_organization_skill_install', { skillId, version }),
    installOrganizationSkill: (skillId: string, version?: string, optionalPluginIds: string[] = []) => invoke<OrganizationSkillInstallResponse>('install_organization_skill', { skillId, version, optionalPluginIds }),
    skillDrafts: () => invoke<AuthoringSkillDraft[]>('list_skill_drafts'),
    skillSubmissions: () => invoke<SkillSubmissionStatus[]>('list_skill_submissions'),
    importSkillCandidate: (revisionOfVersion?: string, parentSubmissionId?: string) => invoke<AuthoringSkillDraft>('import_skill_candidate', { revisionOfVersion, parentSubmissionId }),
    saveSkillDraft: (input: AuthoringSkillDraftInput) => invoke<AuthoringSkillDraft>('save_skill_draft', { input }),
    createSkillRevision: (skillId: string, version: string) => invoke<AuthoringSkillDraft>('create_skill_revision', { skillId, version }),
    testSkillDraft: (skillId: string, version: string) => invoke<AuthoringSkillTestResult>('test_skill_draft', { skillId, version }),
    confirmSkillDraft: (skillId: string, version: string) => invoke<AuthoringSkillDraft>('confirm_skill_draft', { skillId, version }),
    submitSkillDraft: (skillId: string, version: string) => invoke<AuthoringSkillDraft>('submit_skill_draft', { skillId, version }),
    codexSkillStatus: () => invoke<CodexSkillStatusResponse>('get_codex_skill_status'),
    skillSyncSettings: () => invoke<SkillSyncSettings>('get_skill_sync_settings'),
    setSkillSyncMode: (mode: SkillSyncSettings['mode']) => invoke<SkillSyncSettings>('set_skill_sync_mode', { mode }),
    syncCodexSkills: () => invoke<CodexSkillSyncResponse>('sync_codex_skills'),
    syncCodexSkill: (skillId: string) => invoke<CodexSkillActionResponse>('sync_codex_skill', { skillId }),
    syncSkillClient: (skillId: string, clientId: string) => invoke<CodexSkillActionResponse>('sync_skill_client', { skillId, clientId }),
    repairCodexSkill: (skillId: string, preserveModified = true) => invoke<CodexSkillActionResponse>('repair_codex_skill', { skillId, preserveModified }),
    uninstallCodexSkill: (skillId: string) => invoke<CodexSkillUninstallResponse>('uninstall_codex_skill', { skillId }),
    unregisterSkillClient: (skillId: string, clientId: string) => invoke<SkillClientUnregisterResponse>('unregister_skill_client', { skillId, clientId }),
    unregisterSkillClients: (skillId: string) => invoke<SkillClientsUnregisterResponse>('unregister_skill_clients', { skillId }),
    openFolder: (path: string) => invoke('open_folder', { path }),
    installPlugin: (pluginId: string, version?: string) => invoke('install_plugin', { pluginId, version }),
    uninstallPlugin: (pluginId: string) => invoke('uninstall_plugin', { pluginId }),
    rollbackPlugin: (pluginId: string) => invoke('rollback_plugin', { pluginId }),
    setPluginEnabled: (pluginId: string, enabled: boolean) => invoke('set_plugin_enabled', { pluginId, enabled }),
    capabilities: () => invoke<CapabilityItem[]>('get_agent_capabilities'),
    respondApproval: (id: string, approved: boolean) => invoke('respond_approval', { id, approved }),
    setRule: (requestType: string, mode: string) => invoke('set_approval_rule', { requestType, mode }),
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
