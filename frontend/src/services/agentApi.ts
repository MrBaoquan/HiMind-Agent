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
    permissions?: string[];
    capabilities?: { id: string }[];
    views?: PluginViewContribution[];
    commands?: { id: string; title?: string }[];
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
    openPluginView: (pluginId: string, viewId: string) => invoke('open_plugin_view', { pluginId, viewId }),
    createPluginViewShortcut: (pluginId: string, viewId: string, title: string) => invoke('create_plugin_view_shortcut', { pluginId, viewId, title }),
};
