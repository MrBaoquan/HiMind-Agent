import { invoke as tauriInvoke } from '@tauri-apps/api/core';

export type AgentStatus = {
    version: string;
    dashboard_base?: string;
    dashboard_worker_online: boolean;
    dashboard_worker_error?: string;
    dashboard_agent_id?: string;
    local_port?: number;
    mode?: string;
    login_account?: string;
    login_label?: string;
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

export type ApprovalSettings = {
    timeout_seconds: number;
    auto_start: boolean;
    rules?: Record<string, string>;
    editors?: {
        unity_editor_path: string;
        source: 'agent' | 'environment' | 'automatic';
        valid: boolean;
    };
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
    level?: string;
    message?: string;
};

export type PluginRegistry = {
    registry_ready: boolean;
    registry_dir?: string;
    external_runtime?: string;
    total?: number;
    items?: PluginItem[];
};

export type PluginItem = {
    id: string;
    name?: string;
    version?: string;
    runtime?: string;
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
    permissions?: string[];
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
    review_status?: string;
    governance: 'required' | 'managed' | 'optional' | 'blocked';
    version: string;
    release_notes: string;
    min_agent_version: string;
    file_size: number;
    sha256: string;
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
    version: string;
    scope: SkillScope;
    description?: string;
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
    client_state: 'not_installed' | 'installed' | 'outdated' | 'modified' | 'blocked' | 'unsupported' | 'failed';
    installed_version?: string | null;
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
    version: string;
    release_notes: string;
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
};

export type OrganizationSkillInstallResponse = {
    catalog_item: OrganizationSkillCatalogItem;
    record: SkillRecord;
    codex: CodexSkillActionResponse;
};

export type SkillPluginInstallAction = {
    plugin_id: string;
    required: boolean;
    current_version: string;
    target_version: string;
    action: 'satisfied' | 'install' | 'update' | 'blocked' | 'unavailable';
    reason: string;
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
    version: string;
    description: string;
    min_agent_version: string;
    supported_clients: string[];
    capabilities: SkillCapabilityDependency[];
    plugin_dependencies: SkillPluginDependency[];
    risk_summary: string;
    readme: string;
};

export type AuthoringSkillDraft = {
    manifest: SkillManifest;
    readme: string;
    candidate_path: string;
    candidate_sha256: string;
    tested_at?: string | null;
    confirmed_at?: string | null;
    submitted_at?: string | null;
    dashboard_draft_id?: string | null;
    codex_target?: string | null;
    updated_at: string;
};

export type AuthoringSkillTestResult = {
    draft: AuthoringSkillDraft;
    readiness: SkillReadiness;
    plugin_issues: string[];
    codex: CodexSkillActionResponse;
};

export type SkillSubmissionStatus = {
    id: string;
    product_key: string;
    version: string;
    status: 'submitted' | 'approved' | 'changes_requested' | 'rejected';
    review_note?: string;
    artifact_id?: string;
    release_id?: string;
    updated_at: string;
};

export type CodexSkillStatusResponse = {
    client_id: string;
    target_root: string;
    target_source: string;
    target_configured: boolean;
    target_exists: boolean;
    target_mode: 'configured' | 'detected' | 'preview';
    items: CodexSkillStatusItem[];
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
};

export type CodexSkillActionResponse = {
    client_id: string;
    target_root: string;
    target_source?: string;
    target_configured?: boolean;
    rendered: CodexSkillSyncRendered;
    backup_root?: string | null;
};

export function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    return tauriInvoke<T>(command, args);
}

export const agentApi = {
    status: () => invoke<AgentStatus>('get_agent_status'),
    approvals: () => invoke<ApprovalItem[]>('get_pending_approvals'),
    settings: () => invoke<ApprovalSettings>('get_approval_settings'),
    login: () => invoke<LoginState>('get_local_login_status'),
    logs: () => invoke<LogItem[]>('get_agent_logs'),
    plugins: () => invoke<PluginRegistry>('get_plugin_registry'),
    pluginCatalog: () => invoke<PluginCatalogItem[]>('get_plugin_catalog'),
    skillCatalog: () => invoke<SkillCatalogResponse>('get_skill_catalog'),
    organizationSkillCatalog: () => invoke<OrganizationSkillCatalogItem[]>('get_organization_skill_catalog'),
    planOrganizationSkillInstall: (skillId: string) => invoke<SkillInstallPlan>('plan_organization_skill_install', { skillId }),
    installOrganizationSkill: (skillId: string, optionalPluginIds: string[] = []) => invoke<OrganizationSkillInstallResponse>('install_organization_skill', { skillId, optionalPluginIds }),
    skillDrafts: () => invoke<AuthoringSkillDraft[]>('list_skill_drafts'),
    skillSubmissions: () => invoke<SkillSubmissionStatus[]>('list_skill_submissions'),
    saveSkillDraft: (input: AuthoringSkillDraftInput) => invoke<AuthoringSkillDraft>('save_skill_draft', { input }),
    testSkillDraft: (skillId: string, version: string) => invoke<AuthoringSkillTestResult>('test_skill_draft', { skillId, version }),
    confirmSkillDraft: (skillId: string, version: string) => invoke<AuthoringSkillDraft>('confirm_skill_draft', { skillId, version }),
    submitSkillDraft: (skillId: string, version: string) => invoke<AuthoringSkillDraft>('submit_skill_draft', { skillId, version }),
    codexSkillStatus: () => invoke<CodexSkillStatusResponse>('get_codex_skill_status'),
    syncCodexSkills: () => invoke<CodexSkillSyncResponse>('sync_codex_skills'),
    syncCodexSkill: (skillId: string) => invoke<CodexSkillActionResponse>('sync_codex_skill', { skillId }),
    repairCodexSkill: (skillId: string, preserveModified = true) => invoke<CodexSkillActionResponse>('repair_codex_skill', { skillId, preserveModified }),
    uninstallCodexSkill: (skillId: string) => invoke<CodexSkillUninstallResponse>('uninstall_codex_skill', { skillId }),
    openFolder: (path: string) => invoke('open_folder', { path }),
    installPlugin: (pluginId: string) => invoke('install_plugin', { pluginId }),
    uninstallPlugin: (pluginId: string) => invoke('uninstall_plugin', { pluginId }),
    rollbackPlugin: (pluginId: string) => invoke('rollback_plugin', { pluginId }),
    setPluginEnabled: (pluginId: string, enabled: boolean) => invoke('set_plugin_enabled', { pluginId, enabled }),
    capabilities: () => invoke<CapabilityItem[]>('get_agent_capabilities'),
    respondApproval: (id: string, approved: boolean) => invoke('respond_approval', { id, approved }),
    setRule: (requestType: string, mode: string) => invoke('set_approval_rule', { requestType, mode }),
    setTimeout: (seconds: number) => invoke('set_approval_timeout', { seconds }),
    setAutoStart: (enabled: boolean) => invoke<{ auto_start: boolean }>('set_auto_start', { enabled }),
    pickUnityEditor: () => invoke<{ path?: string }>('pick_unity_editor'),
    saveUnityEditor: (path: string) => invoke('save_unity_editor', { path }),
    saveLogin: (username: string, password: string) => invoke<LoginState>('save_local_login', { username, password }),
    logoutLogin: () => invoke<LoginState>('logout_local_login'),
    svnConnections: () => invoke<{ items: SvnConnection[] }>('get_svn_connections'),
    saveSvnConnection: (request: SvnConnectionInput) => invoke<{ connection: SvnConnection }>('save_svn_connection', { request }),
    removeSvnConnection: () => invoke<{ removed: boolean }>('remove_svn_connection'),
    testSvnConnection: () => invoke<SvnConnectionTest>('test_svn_connection'),
    openDashboard: () => invoke('open_dashboard_page'),
    openInnerAdmin: () => invoke('open_inner_admin_page'),
    openAgentDirectory: () => invoke('open_agent_directory'),
    openPluginDirectory: () => invoke('open_plugin_directory'),
    registerDevelopmentPlugin: () => invoke<string>('register_development_plugin'),
    unregisterDevelopmentPlugin: (pluginId: string) => invoke('unregister_development_plugin', { pluginId }),
    invokeDevelopmentPlugin: (pluginId: string, capabilityId: string, input: unknown) => invoke<DevelopmentInvocationResult>('invoke_development_plugin', { pluginId, capabilityId, input }),
    openPluginView: (pluginId: string, viewId: string) => invoke('open_plugin_view', { pluginId, viewId }),
    createPluginViewShortcut: (pluginId: string, viewId: string, title: string) => invoke('create_plugin_view_shortcut', { pluginId, viewId, title }),
};
