import { Suspense, useEffect, useMemo, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { RefreshCw, ShieldAlert } from 'lucide-react';
import '../styles.css';
import { BusyIndicator } from './components/BusyIndicator';
import { ConfirmProvider, useConfirm } from './components/ConfirmDialog';
import { NotificationCenter, PageHeader } from './components/Common';
import { Shell, type ActiveRunSummary } from './components/Shell';
import { SettingsWindow } from './components/SettingsWindow';
import { isSettingsRailKey, settingsRailKey, settingsRailNavigation, settingsRoute, type SettingsSection, type SettingsTab, type SettingsWindowPanel } from './settingsModel';
import { lazyNamed } from './utils/lazyNamed';
import { managedItems } from './utils/managedItems';
const ApprovalsPage = lazyNamed(() => import('./pages/ApprovalsPage'), 'ApprovalsPage');
const AiConnectionsPage = lazyNamed(() => import('./pages/AiConnectionsPage'), 'AiConnectionsPage');
import { clientLabel } from './utils/clientLabels';
import { BuiltinAiPage } from './pages/BuiltinAiPage';
const DashboardPage = lazyNamed(() => import('./pages/DashboardPage'), 'DashboardPage');
import { PluginsPage, userInstalledPlugins } from './pages/PluginsPage';
import { SkillsWorkspacePage, installedSkills, skillClientDescriptors, targetForSkillClient } from './pages/SkillsWorkspacePage';
const InstalledPage = lazyNamed(() => import('./pages/InstalledPage'), 'InstalledPage');
const ExpertStudioPanel = lazyNamed(() => import('./components/ExpertStudioPanel'), 'ExpertStudioPanel');
const InstructionProjectionPanel = lazyNamed(() => import('./components/InstructionProjectionPanel'), 'InstructionProjectionPanel');
const ManagedCapabilitiesPanel = lazyNamed(() => import('./pages/ManagedCapabilitiesPage'), 'ManagedCapabilitiesPanel');
const ExtensionDevelopmentPage = lazyNamed(() => import('./pages/ExtensionDevelopmentPage'), 'ExtensionDevelopmentPage');
const ExtensionsPage = lazyNamed(() => import('./pages/ExtensionsPage'), 'ExtensionsPage');
import type { MarketLoadError } from './pages/ExtensionsPage';
const InboxPage = lazyNamed(() => import('./pages/InboxPage'), 'InboxPage');
const SettingsPage = lazyNamed(() => import('./pages/SettingsPage'), 'SettingsPage');
import { McpConnectionsPanel } from './components/McpConnectionsPanel';
import { useMcpManager } from './components/useMcpManager';
import type { WorkbenchConnectionDraft } from './components/WorkbenchConnectionsPanel';
const TaskCenterPage = lazyNamed(() => import('./pages/TaskCenterPage'), 'TaskCenterPage');
const WorkflowsPage = lazyNamed(() => import('./pages/WorkflowsPage'), 'WorkflowsPage');
const SchedulesPage = lazyNamed(() => import('./pages/SchedulesPage'), 'SchedulesPage');
import { agentApi, type AIServiceListResult, type AIServiceTemplateListResult, type AcpRuntimeProfileSnapshot, type AgentStatus, type AgentUpdateStatus, type AiUsageRange, type ApprovalFact, type ApprovalItem, type ApprovalSettings, type BuiltinAIToolContextSummary, type BuiltinAiWorkspaceTarget, type CapabilityItem, type ClientCapabilityMatrix, type CodexSkillStatusResponse, type CreateExtensionProjectInput, type DashboardAuthorizationProgress, type DashboardIdentityStatus, type ExpertCatalogItem, type ExpertSummary, type ExtensionCollaborationInvitation, type ExtensionProject, type ExtensionProjectKind, type ExtensionProjectSourceInput, type ExtensionRemoteProject, type ExtensionSourceAcquisition, type ExtensionSourceConfig, type ExtensionSourceSettings, type ExtensionSourceSnapshot, type ExtensionWorkspaceEntry, type ExtensionWorkspaceSettings, type InferenceGatewayStatus, type InstructionPackCatalogItem, type InstructionPackDraft, type LocalUsageOverview, type McpConnectionTestResult, type McpTargetDescriptor, type ProjectionSyncStatus, type SkillCatalogResponse, type OrganizationSkillCatalogItem, type AuthoringPluginDraft, type AuthoringSkillDraft, type AuthoringWorkflowDraft, type PluginSubmissionStatus, type SkillSubmissionStatus, type LogItem, type LoginState, type PluginQuickAccessView, type PluginRegistry, type RemoteClientOverview, type RemoteExecutionSettings, type SkillSyncSettings, type SkillWorkspaceStatus, type SvnConnection, type SvnConnectionInput, type WorkbenchConnection, type WorkbenchConnectionsSnapshot, type WorkbenchProbe, type WorkflowCenterSnapshot, type WorkflowRunSnapshot, type WorkflowRunVerification } from './services/agentApi';
import { errorDetail, formatError, type InstalledKind, type NavigationTarget, type PageKey, type UiMessage } from './types';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';

let nextNotificationId = 1;

type AiConnectionsTab = 'mcp' | 'services' | 'acp';

function workspaceLabel(root: string) {
  return root.replace(/[\\/]+$/, '').split(/[\\/]/).pop() || '扩展聚合仓库';
}

function friendlyConnectionError(error: unknown, fallback: string) {
  const detail = errorDetail(error).toLowerCase();
  if (detail.includes('备份并重建')) return '原连接文件格式有误，请选择“备份并重建”。';
  if (detail.includes('permission denied') || detail.includes('access is denied') || detail.includes('拒绝访问')) return '无法修改连接信息，请关闭对应 AI 工具后重试。';
  if (detail.includes('toml') || detail.includes('json') || detail.includes('mcpservers')) return 'AI 工具的连接文件内容有误，请备份后重建。';
  return fallback;
}

function authorizationFailure(progress: DashboardAuthorizationProgress) {
  if (progress.state === 'denied') return '你暂未同意授权，可以重新发起。';
  // 授权页现在会把剩余时间显示给用户；这里负责说清「下一步做什么」，而不是只报一句超时。
  if (progress.state === 'expired') return '这次确认已超时。请重新发起授权，并在浏览器打开的授权页上点「确认授权」。';
  return '未能完成工作台账号授权，请检查网络后重试。';
}

/** 已经翻成用户文案的失败：上层不要再套一层「xx失败：」，免得一句话套两层前缀。 */
class UserFacingError extends Error {}

/**
 * 登记失败要落到「下一步做什么」。工作台回的是 HTTP 状态码和 URL，
 * 直接回显既看不懂也没法行动；原始原因只写控制台。
 */
function enrollFailureText(error: unknown): string {
  const detail = errorDetail(error);
  const lower = detail.toLowerCase();
  if (lower.includes('401') || lower.includes('403') || lower.includes('unauthorized') || lower.includes('forbidden')) {
    return '登记码无效或已过期，请在 HiMind 工作台重新生成后重试。';
  }
  if (lower.includes('404')) return '这个地址不是可用的 HiMind 工作台，请检查工作台地址。';
  if (lower.includes('409') || lower.includes('已登记')) return '这台设备已在工作台登记过，请在工作台确认设备状态。';
  if (lower.includes('timeout') || lower.includes('timed out') || lower.includes('超时')) {
    return '登记超时，请检查网络和工作台地址后重试。';
  }
  if (lower.includes('connection refused') || lower.includes('dns') || lower.includes('network') || lower.includes('网络')) {
    return '无法连接该工作台，请检查地址和网络后重试。';
  }
  // 后端已经把状态翻成产品文案时直接采用（它一定提到「登记」这件事）。
  if (detail.includes('登记')) return detail;
  console.error('工作台登记失败', error);
  return '登记失败，请稍后重试；如果一直失败，请确认工作台地址和登记码。';
}

/**
 * The native window label is the most reliable signal that this document is
 * the dedicated management window. URL parameters and the native bootstrap
 * script are still read for the initial panel, but a missing one of those
 * must never fall back to rendering the main workbench shell here.
 */
function isNativeSettingsWindow(): boolean {
  try {
    return getCurrentWindow().label === 'settings';
  } catch {
    return false;
  }
}

function settingsWindowQuery(): { panel: SettingsWindowPanel; section: SettingsSection; tab: SettingsTab | null; aiTab: AiConnectionsTab } | null {
  try {
    const params = new URLSearchParams(window.location.search);
    const bootstrap = (window as Window & {
      __HIMIND_SETTINGS_WINDOW__?: { panel?: string; section?: string; tab?: string; aiTab?: string };
    }).__HIMIND_SETTINGS_WINDOW__;
    if (params.get('window') !== 'settings' && !bootstrap && !isNativeSettingsWindow()) return null;
    const panel = params.get('panel') || bootstrap?.panel;
    const section = params.get('section') || bootstrap?.section;
    const tab = params.get('tab') || bootstrap?.tab;
    const aiTab = params.get('aiTab') || bootstrap?.aiTab;
    // 旧深链（section=remote / skills / logs……）与未知键位都从同一套规则里落位，
    // 否则设置窗口会静默回到「通用」，入口看起来像坏了。
    const route = settingsRoute({ panel, section, tab });
    return {
      panel: route.panel,
      section: route.section,
      tab: route.tab,
      aiTab: aiTab === 'services' || aiTab === 'acp' ? aiTab : 'mcp',
    };
  } catch {
    return null;
  }
}

function initialPage(): PageKey {
  try {
    const settingsWindow = settingsWindowQuery();
    if (settingsWindow) return settingsWindow.panel;
    const saved = window.localStorage.getItem('himind.page');
    const allowed: PageKey[] = ['dashboard', 'builtin-ai', 'ai', 'approvals', 'inbox', 'tasks', 'workflows', 'schedules', 'extensions', 'installed', 'development', 'settings'];
    // 旧版本把插件、技能各存成一个页面；现在它们是我能力里的页签，读回时落到对应页签。
    if (saved === 'plugins') return 'installed';
    if (saved === 'skills') return 'installed';
    // 运行日志也从独立页面收进了「数据与诊断」页签。
    if (saved === 'logs') return 'settings';
    if (saved && allowed.includes(saved as PageKey)) return saved as PageKey;
  } catch {
    // Webview storage can be unavailable; fall back to the overview.
  }
  // 首次启动直接进入 AI 对话：它才是这个应用的主入口，状态页只是辅助视图。
  return 'builtin-ai';
}

/// 上一次停在「运行日志」的用户，重开设置窗口要落回那个页签，而不是回到通用页。
function legacyLogsLanding(): boolean {
  try {
    return window.localStorage.getItem('himind.page') === 'logs';
  } catch {
    return false;
  }
}

/// 上一次停留在「我的能力」的哪个类型页签：从旧版本升级上来时按旧页面落到对应页签。
function initialInstalledKind(): InstalledKind {
  try {
    const saved = window.localStorage.getItem('himind.page');
    if (saved === 'skills') return 'skill';
    const stored = window.localStorage.getItem('himind-agent.installed-kind');
    if (stored === 'plugin' || stored === 'skill' || stored === 'workflow' || stored === 'expert' || stored === 'mcp' || stored === 'policy') return stored;
  } catch {
    // Webview storage can be unavailable; fall back to plugins.
  }
  return 'plugin';
}

/**
 * 主窗口与设置窗口是同一份前端代码的两个入口，所以内容区在外面由 App 包一层：
 * 站内确认弹窗挂在最外层，两个窗口、任意页面都能共用同一套确认框。
 */
function AgentApp() {
  const settingsWindow = settingsWindowQuery();
  const isSettingsWindow = Boolean(settingsWindow);
  // 破坏性操作统一走站内确认弹窗，不再用 WebView 自带的 window.confirm。
  const confirm = useConfirm();
  const [page, setPage] = useState<PageKey>(initialPage);
  const [installedKind, setInstalledKind] = useState<InstalledKind>(initialInstalledKind);
  const [settingsSection, setSettingsSection] = useState<SettingsSection>(settingsWindow?.section || (legacyLogsLanding() ? 'diagnostics' : 'general'));
  const [settingsTab, setSettingsTab] = useState<SettingsTab | null>(settingsWindow?.tab ?? (legacyLogsLanding() ? 'logs' : null));
  const [status, setStatus] = useState<AgentStatus | null>(null);
  const [projectionSyncStatus, setProjectionSyncStatus] = useState<ProjectionSyncStatus | null>(null);
  const [projectionRequeueBusy, setProjectionRequeueBusy] = useState(false);
  const statusRef = useRef<AgentStatus | null>(null);
  const [updateStatus, setUpdateStatus] = useState<AgentUpdateStatus | null>(null);
  const [updateBusy, setUpdateBusy] = useState(false);
  const [dashboardIdentity, setDashboardIdentity] = useState<DashboardIdentityStatus | null>(null);
  const [dashboardAuthorization, setDashboardAuthorization] = useState<DashboardAuthorizationProgress | null>(null);
  const [workbenchConnections, setWorkbenchConnections] = useState<WorkbenchConnectionsSnapshot | null>(null);
  /** 连接清单读失败时的原因：面板据此从「读取中」转到可重试的错误态。 */
  const [workbenchConnectionsError, setWorkbenchConnectionsError] = useState('');
  // 一次只允许一个连接类操作：切换/登记/移除都会动到同一份本机身份文件。
  const [workbenchBusyId, setWorkbenchBusyId] = useState('');
  const [builtinAiToolContext, setBuiltinAiToolContext] = useState<BuiltinAIToolContextSummary | null>(null);
  // 首屏可能直接落在 HiMind AI 上，激活标记必须跟着初始页走，否则内容区是空的。
  const [builtinAiActivated, setBuiltinAiActivated] = useState(page === 'builtin-ai');
  const [builtinAiWorkspaceRequest, setBuiltinAiWorkspaceRequest] = useState<{ target: BuiltinAiWorkspaceTarget; revision: number }>({ target: null, revision: 0 });
  const [mcpTestResult, setMcpTestResult] = useState<McpConnectionTestResult | null>(null);
  const [mcpTargets, setMcpTargets] = useState<McpTargetDescriptor[]>([]);
  // MCP 工具同一份世界要在市场（获得）、我的能力（拥有）和会话对话框里出现，
  // 所以状态机放主壳里，三处只画同一份数据；只有真看得见的地方才去读写。
  const mcp = useMcpManager({
    active: page === 'extensions' || page === 'installed',
    withCatalog: page === 'extensions',
    onChanged: invalidateBuiltinAiToolContext,
  });
  // 「浏览 MCP 工具」是一次性跳转请求：市场消费掉之后清零，否则每次进市场都会再切一次页签。
  const [openMcpRequest, setOpenMcpRequest] = useState(0);
  const [aiServices, setAiServices] = useState<AIServiceListResult | null>(null);
  const [aiServiceTemplates, setAiServiceTemplates] = useState<AIServiceTemplateListResult | null>(null);
  const [acpRuntimeProfiles, setAcpRuntimeProfiles] = useState<AcpRuntimeProfileSnapshot | null>(null);
  // 用量只做网关这一条口径（ADR 0113）：平台口径留在工作台，不在 Agent 重复展示。
  const [localUsage, setLocalUsage] = useState<LocalUsageOverview | null>(null);
  const [localUsageRange, setLocalUsageRange] = useState<AiUsageRange>('7d');
  const [localUsageBusy, setLocalUsageBusy] = useState(false);
  const [inferenceGateway, setInferenceGateway] = useState<InferenceGatewayStatus | null>(null);
  const [bindingModeBusy, setBindingModeBusy] = useState(false);
  const [gatewayBusy, setGatewayBusy] = useState(false);
  const [aiConnectionsTab, setAiConnectionsTab] = useState<AiConnectionsTab>(settingsWindow?.aiTab || 'mcp');
  const [aiOperation, setAiOperation] = useState<string | null>(null);
  const [approvals, setApprovals] = useState<ApprovalItem[]>([]);
  const [approvalHistory, setApprovalHistory] = useState<ApprovalFact[]>([]);
  const [settings, setSettings] = useState<ApprovalSettings | null>(null);
  const [remoteExecutionSettings, setRemoteExecutionSettings] = useState<RemoteExecutionSettings | null>(null);
  const [remoteClients, setRemoteClients] = useState<RemoteClientOverview | null>(null);
  const [settingsLoading, setSettingsLoading] = useState(true);
  const [settingsLoadError, setSettingsLoadError] = useState('');
  const [loginState, setLoginState] = useState<LoginState | null>(null);
  const [logs, setLogs] = useState<LogItem[]>([]);
  const [pluginRegistry, setPluginRegistry] = useState<PluginRegistry | null>(null);
  const [extensionDesiredState, setExtensionDesiredState] = useState<import('./services/agentApi').ExtensionDesiredState | null>(null);
  const [extensionDesiredError, setExtensionDesiredError] = useState<string | null>(null);
  const [extensionDesiredLoading, setExtensionDesiredLoading] = useState(false);
  const [pluginsLoading, setPluginsLoading] = useState(true);
  const [workflowCenter, setWorkflowCenter] = useState<WorkflowCenterSnapshot | null>(null);
  // 从工作流页“加定时计划”跳过来时预选的定时目标。
  const [schedulePresetTarget, setSchedulePresetTarget] = useState('');
  // 跨页面打开 Workflow Run 时保留目标，避免只能回到列表里手动查找。
  const [workflowRunTarget, setWorkflowRunTarget] = useState('');
  const [workflowLoading, setWorkflowLoading] = useState(true);
  const [workflowError, setWorkflowError] = useState('');
  // 工作流中心的瞬时读失败（启动瞬间控制面还没就绪、快照接口偶发超时）不该把整页刷成
  // 红色错误条：保留上一份快照静默降级，只在连续拿不到数据时才提示。
  const workflowCenterSnapshot = useRef<WorkflowCenterSnapshot | null>(null);
  const workflowCenterFailures = useRef(0);
  useEffect(() => { workflowCenterSnapshot.current = workflowCenter; }, [workflowCenter]);
  const [capabilities, setCapabilities] = useState<CapabilityItem[]>([]);
  const [pluginCatalog, setPluginCatalog] = useState<import('./services/agentApi').PluginCatalogItem[]>([]);
  const [instructionPacks, setInstructionPacks] = useState<InstructionPackCatalogItem[]>([]);
  const [instructionPackError, setInstructionPackError] = useState<string | null>(null);
  const [experts, setExperts] = useState<ExpertSummary[]>([]);
  const [expertCatalog, setExpertCatalog] = useState<ExpertCatalogItem[]>([]);
  const [activeExpert, setActiveExpert] = useState<{ expert_id: string; version: string } | null>(null);
  const [pluginDrafts, setPluginDrafts] = useState<AuthoringPluginDraft[]>([]);
  const [workflowDrafts, setWorkflowDrafts] = useState<AuthoringWorkflowDraft[]>([]);
  const [expertDrafts, setExpertDrafts] = useState<import('./services/agentApi').ExpertAuthoringDraft[]>([]);
  const [instructionDrafts, setInstructionDrafts] = useState<InstructionPackDraft[]>([]);
  const [workflowSubmissions, setWorkflowSubmissions] = useState<AuthoringWorkflowDraft[]>([]);
  const [pluginSubmissions, setPluginSubmissions] = useState<PluginSubmissionStatus[]>([]);
  const [skillCatalog, setSkillCatalog] = useState<SkillCatalogResponse | null>(null);
  const [skillStatus, setSkillStatus] = useState<CodexSkillStatusResponse | null>(null);
  /// 客户端能力矩阵：客户端清单与可用性的唯一来源，技能页与安装计划都读它。
  const [clientMatrix, setClientMatrix] = useState<ClientCapabilityMatrix | null>(null);
  const [skillWorkspace, setSkillWorkspace] = useState<SkillWorkspaceStatus>({ configured: false, valid: false, root: '', workspace_id: '', agents_skills_root: '', lock_path: '', managed_skill_count: 0, error: '' });
  const [organizationSkills, setOrganizationSkills] = useState<OrganizationSkillCatalogItem[]>([]);
  const [skillMarketError, setSkillMarketError] = useState<string | null>(null);
  /// 插件市场目录的独立错误：它来自工作台目录 + 扩展源合并，失败时不能静默为空列表。
  const [pluginCatalogError, setPluginCatalogError] = useState<string | null>(null);
  const [skillDrafts, setSkillDrafts] = useState<AuthoringSkillDraft[]>([]);
  const [skillSubmissions, setSkillSubmissions] = useState<SkillSubmissionStatus[]>([]);
  const [skillError, setSkillError] = useState<string | null>(null);
  // 技能这块的"首读补一次"：冷启动时这两个接口要现扫本机所有 AI 工具目录，
  // 第一次调用慢是正常的，不该把"还没算完"直接渲染成一屏错误。
  const skillCatalogSnapshot = useRef<SkillCatalogResponse | null>(null);
  const skillStatusSnapshot = useRef<CodexSkillStatusResponse | null>(null);
  const skillLoadFailures = useRef(0);
  useEffect(() => { skillCatalogSnapshot.current = skillCatalog; }, [skillCatalog]);
  useEffect(() => { skillStatusSnapshot.current = skillStatus; }, [skillStatus]);
  const [skillOperation, setSkillOperation] = useState<string | null>(null);
  const [extensionProjects, setExtensionProjects] = useState<ExtensionProject[]>([]);
  const [extensionWorkspace, setExtensionWorkspace] = useState<ExtensionWorkspaceSettings>({ configured: false, valid: false, root: '', catalog_path: '', repository: '', default_branch: '', extension_count: 0, error: '' });
  /// 登记过的本机开发目录清单。扩展开发页的左栏就是它，和市场无关，因此单独持有。
  const [extensionWorkspaces, setExtensionWorkspaces] = useState<ExtensionWorkspaceEntry[]>([]);
  const [extensionSources, setExtensionSources] = useState<ExtensionSourceSettings>({ schema_version: 1, sources: [] });
  const [extensionSourceSnapshot, setExtensionSourceSnapshot] = useState<ExtensionSourceSnapshot | null>(null);
  const [extensionSourcesLoading, setExtensionSourcesLoading] = useState(false);
  const [installingUnitKey, setInstallingUnitKey] = useState('');
  const [extensionSourcesRequest, setExtensionSourcesRequest] = useState(0);
  const [extensionSourcesError, setExtensionSourcesError] = useState('');
  const [extensionProjectsError, setExtensionProjectsError] = useState('');
  const [extensionRemoteProjects, setExtensionRemoteProjects] = useState<ExtensionRemoteProject[]>([]);
  const [extensionInvitations, setExtensionInvitations] = useState<ExtensionCollaborationInvitation[]>([]);
  const [developmentOperation, setDevelopmentOperation] = useState<string | null>(null);
  /// 扩展开发页首次数据到位前显示加载态。整页扫盘 + 合并工作区在冷启动需要时间，
  /// 没有这个标记时页面会先画一次"0 个扩展"的空态，看起来像数据丢了。
  const [developmentLoaded, setDevelopmentLoaded] = useState(false);
  const [messages, setMessages] = useState<UiMessage[]>([]);
  const reviewSnapshot = useRef<Map<string, string> | null>(null);
  const [loginModalOpen, setLoginModalOpen] = useState(false);
  const [loginUsername, setLoginUsername] = useState('');
  const [loginPassword, setLoginPassword] = useState('');
  const [svnConnections, setSvnConnections] = useState<SvnConnection[]>([]);
  const svnRefreshInFlight = useRef<Promise<void> | null>(null);
  const [svnModalOpen, setSvnModalOpen] = useState(false);
  const [svnDraft, setSvnDraft] = useState<SvnConnectionInput>({ username: '', password: '' });
  const [svnTesting, setSvnTesting] = useState(false);
  const refreshInFlight = useRef(new Map<string, Promise<unknown>>());
  const notifiedWorkflowApprovals = useRef(new Set<string>());
  const notifiedWorkflowOutcomes = useRef(new Set<string>());
  const workflowOutcomePrimed = useRef(false);

  function singleFlight<T>(key: string, operation: () => Promise<T>, options?: { force?: boolean }): Promise<T> {
    const existing = refreshInFlight.current.get(key) as Promise<T> | undefined;
    if (existing && !options?.force) return existing;
    const current = (async () => {
      // Mutations can finish while a page refresh is still in flight. A forced
      // refresh waits for that stale read before fetching the authoritative
      // state, so the mutation cannot be overwritten by an older response.
      if (existing && options?.force) {
        try { await existing; } catch { /* The new read is still authoritative. */ }
      }
      return operation();
    })().finally(() => {
      if (refreshInFlight.current.get(key) === current) refreshInFlight.current.delete(key);
    });
    refreshInFlight.current.set(key, current);
    return current;
  }

  async function refreshStatus() {
    return singleFlight('status', async () => {
      const next = await agentApi.status();
      statusRef.current = next;
      setStatus(next);
      return next;
    });
  }
  function dashboardEnabled() {
    const current = statusRef.current || status;
    if (!current) return false;
    return current.mode !== 'independent' && current.dashboard_enabled !== false;
  }
  function extensionMarketEnabled() {
    return dashboardEnabled() || extensionSources.sources.some(source => source.enabled);
  }
  async function refreshUpdateStatus() {
    return singleFlight('update-status', async () => { setUpdateStatus(await agentApi.updateStatus()); });
  }
  async function refreshDashboardIdentity() {
    return singleFlight('dashboard-identity', async () => {
      const identity = await agentApi.dashboardIdentity();
      setDashboardIdentity(identity);
      if (identity.state !== 'independent') {
        // identity_status may finish the delayed local SVN bootstrap. Read the file only after it returns.
        try { await refreshSvnConnections(); } catch (error) { console.error(error); }
      }
    });
  }
  async function refreshWorkbenchConnections() {
    return singleFlight('workbench-connections', async () => {
      try {
        setWorkbenchConnections(await agentApi.workbenchConnections());
        setWorkbenchConnectionsError('');
      } catch (error) {
        // 连接清单读不到时不要把上一次的结果当成现状；面板据此给出「读取失败 + 重新读取」，
        // 而不是一直停在「读取中」——那是加载态，不是错误态。
        console.error('工作台连接读取失败', error);
        setWorkbenchConnections(null);
        setWorkbenchConnectionsError(formatError(error, '工作台连接读取失败'));
      }
    });
  }
  async function refreshProjectionSyncStatus() {
    return singleFlight('projection-sync-status', async () => {
      setProjectionSyncStatus(await agentApi.projectionSyncStatus());
    });
  }
  /// 重投只把失败记录放回队列，真正上报由投影循环在 30 秒内接手，所以这里刷新到的是「已重新排队」的状态。
  async function requeueProjectionDeadLetters() {
    if (projectionRequeueBusy) return;
    setProjectionRequeueBusy(true);
    try {
      const report = await agentApi.requeueProjectionDeadLetters();
      await refreshProjectionSyncStatus();
      if (!report.requeued) {
        notify('info', '没有可重新同步的记录');
      } else if (report.dead_letter_after === 0) {
        notify('success', `已重新排队 ${report.requeued} 条同步记录`);
      } else {
        notify('info', `已重新排队 ${report.requeued} 条，仍有 ${report.dead_letter_after} 条需要处理`);
      }
    } catch (error) {
      notify('error', formatError(error, '重新同步失败'));
    } finally {
      setProjectionRequeueBusy(false);
    }
  }
  async function refreshMcpTargets() {
    return singleFlight('mcp-targets', async () => { setMcpTargets(await agentApi.mcpTargets()); });
  }
  async function refreshAIServices() {
    return singleFlight('ai-services', async () => { setAiServices(await agentApi.listAIServices()); });
  }
  /**
   * 本机用量与网关状态一起读：面板需要同时知道「有没有数据」和
   * 「哪些客户端根本没走网关」，否则会把看不见读成没消耗。
   */
  async function refreshLocalUsage(options?: { force?: boolean; range?: AiUsageRange }) {
    const range = options?.range ?? localUsageRange;
    setLocalUsageBusy(true);
    try {
      await singleFlight(`local-usage:${range}`, async () => {
        const [usage, gateway] = await Promise.all([
          agentApi.localUsageOverview(range),
          agentApi.inferenceGatewayStatus(),
        ]);
        setLocalUsage(usage);
        setInferenceGateway(gateway);
      }, { force: options?.force });
    } catch (error) {
      console.error('本机用量读取失败', error);
      setLocalUsage(null);
    } finally {
      setLocalUsageBusy(false);
    }
  }
  function changeLocalUsageRange(range: AiUsageRange) {
    setLocalUsageRange(range);
    void refreshLocalUsage({ range, force: true });
  }
  /**
   * P1 的最小入口：把 Codex 切到本机网关。真正的「服务 × 客户端」矩阵
   * 属 P2（ADR 0113 分期），这里先让这条链路可用、可验证。
   */
  async function bindCodexToGateway() {
    setBindingModeBusy(true);
    try {
      await setClientBindingMode('codex', 'gateway');
    } finally {
      setBindingModeBusy(false);
    }
  }
  /** 注入模式切换：客户端配置、网关绑定与用量口径会同时变化，一起回读。 */
  async function setClientBindingMode(target: string, mode: 'gateway' | 'direct', service?: string) {
    try {
      await agentApi.setProviderBindingMode(target, mode, service);
      notify('success', mode === 'gateway' ? `${target} 已切换到本机网关` : `${target} 已切回直连`);
      await Promise.all([refreshInferenceGateway(), refreshAIServices(), refreshLocalUsage({ force: true })]);
    } catch (error) {
      notify('error', formatError(error, mode === 'gateway' ? '切换到本机网关失败' : '切回直连失败'));
    }
  }
  async function refreshInferenceGateway() {
    try {
      setInferenceGateway(await agentApi.inferenceGatewayStatus());
    } catch (error) {
      console.error('本机网关状态读取失败', error);
    }
  }
  /**
   * 重启用于端口被释放或异常退出之后的恢复；失败不弹成功，只把真实原因带回界面。
   */
  async function restartGateway() {
    setGatewayBusy(true);
    try {
      setInferenceGateway(await agentApi.restartInferenceGateway());
      notify('success', '本机网关已重启');
    } catch (error) {
      notify('error', formatError(error, '重启本机网关失败'));
      await refreshInferenceGateway();
    } finally {
      setGatewayBusy(false);
    }
  }
  /**
   * 停用网关必须先把客户端切回直连：先停监听会留下一批指向空端口、
   * 连不上上游的客户端，而用户看不出这两件事的因果关系。
   */
  async function stopGatewayAndUnbind() {
    const affected = inferenceGateway?.gateway_clients.length ?? 0;
    const accepted = await confirm({
      title: '停用本机网关？',
      description: affected
        ? `会把 ${affected} 个走网关的工具切回直连，然后停止监听。`
        : '当前没有工具走网关，将只停止监听。',
      confirmText: '停用',
    });
    if (!accepted) return;
    setGatewayBusy(true);
    try {
      const report = await agentApi.stopInferenceGatewayAndUnbind();
      if (report.failures.length) {
        notify('error', `${report.failures.length} 个工具没能切回直连，网关保持运行`);
      } else {
        notify('success', report.stopped ? '本机网关已停用' : '本机网关本来就没有运行');
      }
      await Promise.all([refreshInferenceGateway(), refreshAIServices(), refreshLocalUsage({ force: true })]);
    } catch (error) {
      notify('error', formatError(error, '停用本机网关失败'));
    } finally {
      setGatewayBusy(false);
    }
  }
  // 预设目录读不到时回落内置兜底列表，不让它拖垮 AI 页其他数据。
  async function refreshAIServiceTemplates() {
    return singleFlight('ai-service-templates', async () => {
      try {
        setAiServiceTemplates(await agentApi.listAIServiceTemplates());
      } catch (error) {
        console.error('AI 服务预设目录读取失败，改用内置兜底列表', error);
        setAiServiceTemplates(null);
      }
    });
  }
  async function refreshAcpRuntimeProfiles() {
    return singleFlight('acp-runtime-profiles', async () => {
      setAcpRuntimeProfiles(await agentApi.acpRuntimeProfiles());
    });
  }
  async function refreshBuiltinAiToolContext() {
    return singleFlight('builtin-ai-tool-context', async () => {
      try {
        setBuiltinAiToolContext(await agentApi.builtinAiToolContextSummary());
      } catch {
        setBuiltinAiToolContext(null);
      }
    });
  }
  async function refreshApprovals() {
    return singleFlight('approvals', async () => {
      const [pending, history] = await Promise.all([agentApi.approvals(), agentApi.approvalHistory()]);
      setApprovals(pending);
      setApprovalHistory(history);
    });
  }
  async function refreshSettings() { setSettings(await agentApi.settings()); }
  async function refreshRemoteExecutionSettings() { setRemoteExecutionSettings(await agentApi.remoteExecutionSettings()); }
  async function refreshLogin() { setLoginState(await agentApi.login()); }
  async function refreshSettingsPageData() {
    return singleFlight('settings-page', async () => {
      setSettingsLoading(true);
      setSettingsLoadError('');
      const independent = statusRef.current?.mode === 'independent' || statusRef.current?.dashboard_enabled === false;
      // 账号设置页的主数据是连接清单，和审批设置并行拉，谁先到谁先渲染。
      void refreshWorkbenchConnections();
      try {
        const [settingsResult, remoteExecutionResult, loginResult, remoteClientsResult] = await Promise.allSettled([
          withTimeout(agentApi.settings(), '审批设置'),
          withTimeout(agentApi.remoteExecutionSettings(), '远程任务设置'),
          withTimeout(agentApi.login(), '本地登录状态'),
          withTimeout(agentApi.remoteClients(), '远控工具配置'),
        ] as const);
        const errors: string[] = [];
        if (settingsResult.status === 'fulfilled') setSettings(settingsResult.value);
        else {
          setSettings(null);
          errors.push(formatError(settingsResult.reason, '审批设置读取失败'));
        }
        if (remoteExecutionResult.status === 'fulfilled') setRemoteExecutionSettings(remoteExecutionResult.value);
        else {
          if (independent) {
            setRemoteExecutionSettings({ enabled: false, access_mode: 'exhibit_linked', default_provider: 'himind.builtin' });
          } else {
            setRemoteExecutionSettings(null);
            errors.push(formatError(remoteExecutionResult.reason, '远程任务设置读取失败'));
          }
        }
        if (loginResult.status === 'fulfilled') setLoginState(loginResult.value);
        else if (independent) {
          setLoginState({ status: 'local_only' });
        } else {
          setLoginState(null);
          errors.push(formatError(loginResult.reason, '本地登录状态读取失败'));
        }
        if (remoteClientsResult.status === 'fulfilled') setRemoteClients(remoteClientsResult.value);
        else setRemoteClients({ items: [] });
        setSettingsLoadError(errors.join('；'));
      } catch (error) {
        setSettings(null);
        setRemoteExecutionSettings(null);
        setRemoteClients(null);
        setLoginState(null);
        setSettingsLoadError(formatError(error, '应用设置读取失败'));
      } finally {
        setSettingsLoading(false);
      }
    });
  }
  async function refreshLogs(force = false) {
    return singleFlight('logs', async () => {
      setLogs(await agentApi.logs());
    }, { force });
  }
  async function refreshExtensionProjects() {
    try {
      setExtensionProjects(await agentApi.extensionProjects());
      setExtensionProjectsError('');
    } catch (error) {
      setExtensionProjectsError(formatError(error, '扩展项目读取失败'));
      throw error;
    }
    try { setExtensionRemoteProjects(await agentApi.extensionCollaborationProjects()); }
    catch { setExtensionRemoteProjects([]); }
  }
  async function refreshExtensionInvitations() {
    try { setExtensionInvitations(await agentApi.extensionCollaborationInvitations()); }
    catch { setExtensionInvitations([]); }
  }
  async function refreshSvnConnections() {
    if (svnRefreshInFlight.current) return svnRefreshInFlight.current;
    const operation = (async () => {
      setSvnConnections((await agentApi.svnConnections()).items || []);
    })();
    svnRefreshInFlight.current = operation;
    try {
      await operation;
    } finally {
      if (svnRefreshInFlight.current === operation) svnRefreshInFlight.current = null;
    }
  }
  async function testSvnConnection() {
    if (svnTesting) return;
    setSvnTesting(true);
    try {
      const result = await agentApi.testSvnConnection();
      await refreshSvnConnections();
      notify('success', result.revision ? `SVN 连接成功，当前版本 ${result.revision}` : 'SVN 连接成功');
    } catch (error) {
      try { await refreshSvnConnections(); } catch { /* keep the original test error */ }
      notify('error', formatError(error, '测试 SVN 连接失败'));
    } finally {
      setSvnTesting(false);
    }
  }
  async function refreshPlugins() {
    return singleFlight('plugins', async () => {
      setPluginsLoading(true);
      try {
        // Registry, capability list and market catalog fail independently: a slow
        // capabilities call must not leave the market catalog empty.
        const instructionCatalogPromise = dashboardEnabled()
          ? withTimeout(agentApi.instructionPackCatalog(), '项目规则市场', 30000)
          : Promise.resolve([] as InstructionPackCatalogItem[]);
        const [registryResult, capabilityResult, catalogResult, instructionCatalogResult] = await Promise.allSettled([
          withTimeout(agentApi.plugins(), '本机插件'),
          withTimeout(agentApi.capabilities(), '本机能力清单'),
          // 目录需要等待 Dashboard 侧返回，慢于常规本地读取；12s 会把它整体丢弃。
          withTimeout(agentApi.pluginCatalog(), '插件市场', 30000),
          instructionCatalogPromise,
        ]);
        if (registryResult.status === 'fulfilled') {
          setPluginRegistry(registryResult.value);
        } else {
          setPluginRegistry(null);
          console.error('Plugin registry unavailable', registryResult.reason);
        }
        if (capabilityResult.status === 'fulfilled') {
          setCapabilities(Array.isArray(capabilityResult.value) ? capabilityResult.value : []);
        } else {
          setCapabilities([]);
          console.error('Capability list unavailable', capabilityResult.reason);
        }
        if (catalogResult.status === 'fulfilled') {
          setPluginCatalog(Array.isArray(catalogResult.value) ? catalogResult.value : []);
          setPluginCatalogError(null);
        } else {
          // 目录失败必须说出来：静默保留上一份（或空）列表会让市场看起来"没有可安装的东西"。
          setPluginCatalogError(formatError(catalogResult.reason, '插件市场暂不可用'));
          console.error('Plugin catalog unavailable', catalogResult.reason);
        }
        // 项目规则目录与其它市场数据并行读取。独立模式返回空列表，
        // 但不应把工作台连接错误暴露成市场页错误。
        if (instructionCatalogResult.status === 'fulfilled') {
          setInstructionPacks(Array.isArray(instructionCatalogResult.value) ? instructionCatalogResult.value : []);
          setInstructionPackError(null);
        } else {
          // 保留上一份目录，错误提示单独呈现，避免网络抖动把市场闪成空态。
          setInstructionPackError(formatError(instructionCatalogResult.reason, '项目规则市场暂不可用'));
          console.error('InstructionPack catalog unavailable', instructionCatalogResult.reason);
        }
      } finally {
        setPluginsLoading(false);
      }
    });
  }

  async function refreshExperts(force = false) {
    return singleFlight('experts', async () => {
      const [items, active, catalog] = await Promise.all([agentApi.experts(), agentApi.activeExpert(), agentApi.expertCatalog().catch(() => [])]);
      setExperts(Array.isArray(items) ? items : []);
      setActiveExpert(active ? { expert_id: active.expert_id, version: active.version } : null);
      setExpertCatalog(Array.isArray(catalog) ? catalog : []);
    }, { force });
  }

  async function refreshWorkflowCenter(light = false, force = false) {
    // 轮询用轻量快照：完整快照会去控制面拉工作流目录，不能每个轮询周期都打网络。
    // Light and full reads share one flight. Mutations opt into a forced full
    // read so a poll that started before the mutation cannot win the race.
    return singleFlight('workflow-center', async () => {
      setWorkflowLoading(true);
      try {
        const next = await withTimeout(agentApi.workflowCenter(light), '工作流中心');
        workflowCenterFailures.current = 0;
        setWorkflowError('');
        // 轻量快照不包含目录、工作流定义或投影明细。保留完整快照，
        // 只替换运行状态，避免轮询反复解析磁盘和重绘整页。
        setWorkflowCenter(current => {
          if (!light || !current) return next;
          const previousByRunId = new Map(current.runs.map(item => [item.run.run_id, item]));
          return {
            ...next,
            catalog: current.catalog,
            catalog_error: current.catalog_error,
            workflows: current.workflows,
            library_issues: current.library_issues,
            runs: next.runs.map(run => {
              const previous = previousByRunId.get(run.run.run_id);
              return previous ? {
                ...run,
                workflow_name: run.workflow_name || previous.workflow_name,
                projection_count: run.projection_count || previous.projection_count,
                projection_status: run.projection_status === 'none' ? previous.projection_status : run.projection_status,
              } : run;
            }),
          };
        });
      } catch (error) {
        // 已经有快照就保留它：列表停在上一份数据上，远好过整页变成红色错误条。
        workflowCenterFailures.current += 1;
        const hasSnapshot = workflowCenterSnapshot.current !== null;
        if (!hasSnapshot && workflowCenterFailures.current === 1) {
          // 首读落空先自己补一次，成功的话用户根本看不到错误。
          window.setTimeout(() => { void refreshWorkflowCenter(); }, 1500);
        } else if (!hasSnapshot || workflowCenterFailures.current >= 3) {
          setWorkflowError(formatError(error, '工作流中心读取失败'));
        }
      } finally {
        setWorkflowLoading(false);
      }
    }, { force });
  }

  async function loadWorkflowRun(runId: string): Promise<WorkflowRunSnapshot> {
    return withTimeout(agentApi.workflowRun(runId), '工作流运行详情');
  }

  // One extension distribution unit can carry plugins, skills and workflows at
  // once, so every surface that lists installed extensions has to refresh
  // together. Refreshing only the kind that was "expected" is how the Workflow
  // list silently went stale after a unit install.
  async function refreshExtensionSurfaces() {
    await Promise.all([refreshPlugins(), refreshSkills(), refreshWorkflowCenter(false, true)]);
  }

  async function verifyWorkflowRun(runId: string): Promise<WorkflowRunVerification> {
    return withTimeout(agentApi.verifyWorkflowRun(runId), '工作流运行验证');
  }

  async function approveWorkflowRun(runId: string, stepId: string) {
    await agentApi.approveWorkflowStep(runId, stepId);
    notify('success', '审批已批准，工作流将由统一审批流程自动继续');
    await refreshApprovals();
    await refreshWorkflowCenter(false, true);
  }

  async function rejectWorkflowRun(runId: string, stepId: string) {
    await agentApi.rejectWorkflowStep(runId, stepId);
    notify('info', '工作流审批已拒绝');
    await refreshApprovals();
    await refreshWorkflowCenter(false, true);
  }

  async function resumeWorkflowRun(runId: string, feedback: string) {
    const outcome = await agentApi.resumeWorkflowRun(runId, feedback);
    notify('success', outcome.blocked_step_id ? '已继续执行，正在等待下一步处理' : '工作流已继续执行');
    await refreshWorkflowCenter(false, true);
  }

  async function startWorkflowRun(packageId: string, input: Record<string, unknown>) {
    const outcome = await agentApi.startWorkflowRun(packageId, input);
    await refreshWorkflowCenter(false, true);
    // 启动失败要当场说清楚：等一次必然失败的执行、再去运行详情里翻错误，
    // 是上一版最难受的地方。
    if (outcome.run.status === 'failed') {
      notify('error', outcome.run.error || '工作流启动后立即失败');
    } else if (outcome.blocked_step_id) {
      notify('info', `工作流已启动，等待 ${outcome.blocked_step_id}`);
    } else {
      // 启动现在是「受理即返回」：这里只说受理，过程交给运行详情的实时面板。
      notify('success', '已受理，正在执行');
    }
    return outcome.run;
  }

  async function installWorkflowCatalogItem(workflowId: string, version?: string, source?: string, artifactId?: string, sha256?: string) {
    setWorkflowLoading(true);
    try {
      await agentApi.installWorkflowCatalogItem(workflowId, version, source, artifactId, sha256);
      notify('success', version ? `工作流已更新到 v${version}` : '工作流已安装');
      await refreshWorkflowCenter(false, true);
    } catch (error) {
      notify('error', formatError(error, '安装工作流失败'));
      throw error;
    } finally {
      setWorkflowLoading(false);
    }
  }

  async function cancelWorkflowRun(runId: string) {
    await agentApi.cancelWorkflowRun(runId);
    notify('info', '工作流已取消');
    await refreshWorkflowCenter(false, true);
  }

  // Plugin views can be installed or rebuilt while the Agent window remains
  // open.  Keep the navigation registry fresh without reloading the whole
  // dashboard state, so newly registered quick entries become discoverable
  // as soon as the window is focused again.
  async function refreshPluginRegistry() {
    return singleFlight('plugin-registry', async () => {
      try {
        const [registry, capabilityItems] = await Promise.all([
          withTimeout(agentApi.plugins(), '本机插件'),
          withTimeout(agentApi.capabilities(), '本机能力清单'),
        ]);
        setPluginRegistry(registry);
        setCapabilities(Array.isArray(capabilityItems) ? capabilityItems : []);
      } catch (error) {
        console.error('Plugin registry refresh unavailable', error);
      }
    });
  }

  async function refreshExtensionDesiredState() {
    if (!dashboardEnabled()) {
      setExtensionDesiredState(null);
      setExtensionDesiredError(null);
      setExtensionDesiredLoading(false);
      return;
    }
    return singleFlight('extension-desired-state', async () => {
      setExtensionDesiredLoading(true);
      try {
        setExtensionDesiredState(await withTimeout(agentApi.extensionDesiredState(), '系统内置策略'));
        setExtensionDesiredError(null);
      } catch (error) {
        setExtensionDesiredState(null);
        setExtensionDesiredError(formatError(error, '系统内置策略读取失败'));
      } finally {
        setExtensionDesiredLoading(false);
      }
    });
  }

  async function refreshSkills() {
    return singleFlight('skills', async () => {
      const [catalogResult, statusResult, marketResult, workspaceResult, matrixResult] = await Promise.allSettled([
      withTimeout(agentApi.skillCatalog(), '本地技能目录', 20000),
      withTimeout(agentApi.codexSkillStatus(), 'AI 工具技能状态', 20000),
      withTimeout(agentApi.organizationSkillCatalog(), '技能市场', 30000),
      withTimeout(agentApi.skillWorkspace(), '项目技能目录'),
      withTimeout(agentApi.clientCapabilityMatrix(), 'AI 工具能力矩阵'),
    ]);
    const errors: string[] = [];
    // 读失败保留上一次结果：一次抖动不该把已经看到的技能列表清空。
    if (catalogResult.status === 'fulfilled') setSkillCatalog(catalogResult.value);
    if (statusResult.status === 'fulfilled') setSkillStatus(statusResult.value);
    const readFailed = catalogResult.status === 'rejected' || statusResult.status === 'rejected';
    const neverLoaded = skillCatalogSnapshot.current === null && skillStatusSnapshot.current === null;
    if (readFailed && neverLoaded && skillLoadFailures.current === 0) {
      // 首读落空先自己补一次：成功的话用户根本看不到错误。
      skillLoadFailures.current += 1;
      window.setTimeout(() => { void refreshSkills(); }, 1500);
    } else {
      if (readFailed) skillLoadFailures.current += 1;
      else skillLoadFailures.current = 0;
      if (catalogResult.status === 'rejected') errors.push(formatError(catalogResult.reason, '本机技能读取失败'));
      if (statusResult.status === 'rejected') errors.push(formatError(statusResult.reason, 'AI 工具技能状态读取失败'));
    }
	if (marketResult.status === 'fulfilled') {
	  setOrganizationSkills(Array.isArray(marketResult.value) ? marketResult.value : []);
	  setSkillMarketError(null);
    } else {
      // Keep the last usable market snapshot while exposing the read error.
	  setSkillMarketError(formatError(marketResult.reason, '技能市场暂不可用'));
	}
	if (workspaceResult.status === 'fulfilled') setSkillWorkspace(workspaceResult.value);
	else setSkillWorkspace(current => ({ ...current, error: formatError(workspaceResult.reason, '项目技能目录读取失败') }));
	// 能力矩阵只是补充来源，读不到就退回技能状态里的推断，不打扰用户。
	setClientMatrix(matrixResult.status === 'fulfilled' ? matrixResult.value : null);
      setSkillError(errors.length ? errors.join('；') : null);
    });
  }

  async function refreshDevelopment(force = false) {
    return singleFlight('development', async () => {
      try {
      const [projects, pluginDraftResult, skillDraftResult, workflowDraftResult, expertDraftResult, instructionDraftResult] = await Promise.allSettled([
      withTimeout(agentApi.extensionProjects(), '扩展项目'),
      withTimeout(agentApi.pluginDrafts(), '插件草稿'),
      withTimeout(agentApi.skillDrafts(), '技能草稿'),
      withTimeout(agentApi.workflowDrafts(), '工作流草稿'),
      withTimeout(agentApi.expertDrafts(), '专家草稿'),
      withTimeout(agentApi.instructionPackDrafts(), '项目规则草稿'),
    ]);
    try { setExtensionWorkspace(await agentApi.extensionWorkspace()); }
    catch (error) { console.error('Extension workspace unavailable', error); }
    // 工作区清单是扩展开发页左栏的直接数据源；读失败时保留上一次结果，
    // 免得一次抖动就把左栏清空（页面上另有项目列表错误提示）。
    try { setExtensionWorkspaces(await agentApi.extensionWorkspaces()); }
    catch (error) { console.error('Extension workspaces unavailable', error); }
    if (projects.status === 'fulfilled') {
      setExtensionProjects(projects.value || []);
      setExtensionProjectsError('');
    } else {
      // Keep the failure visible on the page: the project list is the entry
      // point of the development workspace, so a silent empty list looks like
      // "no projects" instead of a broken Agent call.
      setExtensionProjectsError(
        formatError(projects.reason, '扩展项目读取失败'),
      );
    }
    if (pluginDraftResult.status === 'fulfilled') setPluginDrafts(pluginDraftResult.value || []);
    if (skillDraftResult.status === 'fulfilled') setSkillDrafts(skillDraftResult.value || []);
    if (workflowDraftResult.status === 'fulfilled') setWorkflowDrafts(workflowDraftResult.value || []);
    if (expertDraftResult.status === 'fulfilled') setExpertDrafts(expertDraftResult.value || []);
    if (instructionDraftResult.status === 'fulfilled') setInstructionDrafts(instructionDraftResult.value || []);
    if (!dashboardEnabled()) {
      setExtensionRemoteProjects([]);
      setPluginSubmissions([]);
      setSkillSubmissions([]);
      setWorkflowSubmissions([]);
      setExtensionInvitations([]);
        return;
    }
    const [remoteProjects, pluginSubmissionResult, skillSubmissionResult, workflowSubmissionResult, invitationResult] = await Promise.allSettled([
      withTimeout(agentApi.extensionCollaborationProjects(), '协作项目'),
      withTimeout(agentApi.pluginSubmissions(), '插件审核状态'),
      withTimeout(agentApi.skillSubmissions(), '技能审核状态'),
      withTimeout(agentApi.workflowSubmissions(), '工作流审核状态'),
      withTimeout(agentApi.extensionCollaborationInvitations(), '协作邀请'),
    ]);
    if (remoteProjects.status === 'fulfilled') setExtensionRemoteProjects(remoteProjects.value || []);
    if (pluginSubmissionResult.status === 'fulfilled') setPluginSubmissions(pluginSubmissionResult.value || []);
    if (skillSubmissionResult.status === 'fulfilled') setSkillSubmissions(skillSubmissionResult.value || []);
    if (workflowSubmissionResult.status === 'fulfilled') setWorkflowSubmissions(workflowSubmissionResult.value.items || []);
      if (invitationResult.status === 'fulfilled') setExtensionInvitations(invitationResult.value || []);
      } finally {
        // 无论成功、失败还是未连接工作台的提前返回，都要退出加载态，
        // 否则页面会一直停在"正在读取工作区和扩展"。
        setDevelopmentLoaded(true);
      }
    }, { force });
  }
  async function refreshExtensionSourceSettings(force = false) {
    return singleFlight('extension-source-settings', async () => {
      setExtensionSources(await agentApi.extensionSources());
    }, { force });
  }
  async function refreshExtensionSources() {
    setExtensionSourcesLoading(true);
    setExtensionSourcesError('');
    try {
      const settings = await agentApi.extensionSources();
      setExtensionSources(settings);
      try {
        const snapshot = await agentApi.extensionSourceSnapshot();
        setExtensionSourceSnapshot(snapshot);
        if (Array.isArray(snapshot.experts) && snapshot.experts.length) {
          setExpertCatalog(current => {
            const merged = new Map(current.map(item => [`${item.expert_id}@${item.version}@${item.source}`, item]));
            snapshot.experts.forEach(item => merged.set(`${item.expert_id}@${item.version}@${item.source}`, item));
            return [...merged.values()];
          });
        }
      } catch (error) {
        // 刷新失败时保留上一份可用快照：来源列表、分发单元都从快照派生，
        // 清空会让整个「来源管理」看起来"一条来源都没有"，比数据稍旧更糟。
        setExtensionSourcesError(formatError(error, '来源刷新失败'));
      }
    } finally {
      setExtensionSourcesLoading(false);
    }
  }
  async function addExtensionSource(name: string, repository: string, reference: string, catalogPath: string, verification: ExtensionSourceConfig['verification']) {
    setExtensionSourcesLoading(true);
    try {
      setExtensionSources(await agentApi.addExtensionSource(name, repository, reference, catalogPath, verification));
      await refreshExtensionSources();
      await refreshExtensionSurfaces();
    } finally {
      setExtensionSourcesLoading(false);
    }
  }
  async function updateExtensionSource(source: ExtensionSourceConfig, enabled: boolean, autoUpdate: boolean, verification: ExtensionSourceConfig['verification']) {
    setExtensionSourcesLoading(true);
    try {
      setExtensionSources(await agentApi.updateExtensionSource(source.id, enabled, autoUpdate, verification));
      await refreshExtensionSources();
      await refreshExtensionSurfaces();
    } finally {
      setExtensionSourcesLoading(false);
    }
  }
  async function removeExtensionSource(sourceId: string) {
    setExtensionSourcesLoading(true);
    try {
      setExtensionSources(await agentApi.removeExtensionSource(sourceId));
      await refreshExtensionSources();
      await refreshExtensionSurfaces();
    } finally {
      setExtensionSourcesLoading(false);
    }
  }
  async function setExtensionUnitAcquisition(unitKey: string, acquisition: ExtensionSourceAcquisition) {
    setExtensionSourcesLoading(true);
    try {
      setExtensionSources(await agentApi.setExtensionUnitAcquisition(unitKey, acquisition));
      await refreshExtensionSources();
    } finally {
      setExtensionSourcesLoading(false);
    }
  }
  async function installExtensionUnit(unitKey: string, sourceId: string) {
    setExtensionSourcesLoading(true);
    setInstallingUnitKey(unitKey);
    try {
      const report = await agentApi.installExtensionUnit(unitKey, sourceId);
      await refreshExtensionSources();
      await refreshExtensionSurfaces();
      const origin = report.acquisition === 'remote' ? '发布版本' : '本地开发';
      if (report.errors.length) {
        const installedCount = report.plugins.length + report.skills.length + report.workflows.length;
        const failedCount = report.failures?.length || report.errors.length;
        const retryHint = report.retryable ? '可刷新后重试。' : '';
        notify('error', `从${origin}完成 ${installedCount} 项，${failedCount} 项失败：${report.errors.join('；')}${retryHint}`);
        return;
      }
      // A unit may carry any mix of the three extension kinds, so report each
      // kind instead of summing only the ones this screen happened to expect.
      const installed = [
        report.plugins.length ? `${report.plugins.length} 插件` : '',
        report.skills.length ? `${report.skills.length} 技能` : '',
        report.workflows.length ? `${report.workflows.length} 工作流` : '',
      ].filter(Boolean);
      notify('success', installed.length ? `已从${origin}安装 ${installed.join(' · ')}` : '当前来源没有可安装的扩展');
    } finally {
      setExtensionSourcesLoading(false);
      setInstallingUnitKey('');
    }
  }
  async function switchExtensionWorkspace(root: string) {
    const workspace = await agentApi.setExtensionWorkspace(root);
    setExtensionWorkspace(workspace);
    await refreshDevelopment();
    notify('success', `已切换开发工作区，共 ${workspace.extension_count} 个扩展`);
  }

  async function refreshReviewProgress() {
    if (!dashboardEnabled()) return;
    const [pluginResult, skillResult, projectResult, remoteProjectResult] = await Promise.allSettled([
      agentApi.pluginSubmissions(),
      agentApi.skillSubmissions(),
      agentApi.extensionProjects(),
      agentApi.extensionCollaborationProjects(),
    ]);
    if (pluginResult.status === 'fulfilled') setPluginSubmissions(pluginResult.value || []);
    if (skillResult.status === 'fulfilled') setSkillSubmissions(skillResult.value || []);
    if (projectResult.status === 'fulfilled') setExtensionProjects(projectResult.value || []);
    if (remoteProjectResult.status === 'fulfilled') setExtensionRemoteProjects(remoteProjectResult.value || []);
  }

  async function refreshInitialPage() {
    try {
      await refreshStatus();
    } catch (error) {
      console.error('Agent status unavailable during initialization', error);
    }
    // Settings is a separate native window sharing this entry point. It loads
    // only its selected surface and must not duplicate the main window's
    // global catalogs, approvals, and workflow reads.
    if (isSettingsWindow) return;
    const results = await Promise.allSettled([
      refreshUpdateStatus(),
      refreshDashboardIdentity(),
      refreshWorkbenchConnections(),
      refreshMcpTargets(),
      refreshAIServices(),
      refreshAIServiceTemplates(),
      refreshApprovals(),
      refreshRemoteExecutionSettings(),
      refreshExtensionSourceSettings(),
      refreshPluginRegistry(),
      refreshWorkflowCenter(),
      refreshExperts(),
    ]);
    for (const result of results) {
      if (result.status === 'rejected') throw result.reason;
    }
  }

  useEffect(() => {
    if (isSettingsWindow) return;
    refreshInitialPage().catch(error => notify('error', formatError(error, '应用初始化失败')));
    const timer = window.setInterval(() => {
      if (document.visibilityState !== 'hidden') Promise.all([refreshStatus(), refreshApprovals(), refreshWorkflowCenter(true), refreshProjectionSyncStatus()]).catch(console.error);
    }, 10000);
    const updateTimer = window.setInterval(() => {
      if (document.visibilityState !== 'hidden') refreshUpdateStatus().catch(console.error);
    }, 60000);
    return () => {
      window.clearInterval(timer);
      window.clearInterval(updateTimer);
    };
  }, []);

  useEffect(() => {
    if (!isSettingsWindow) return;
    let unlisten: (() => void) | undefined;
    void listen<{ panel?: string; section?: string; tab?: string; aiTab?: string }>('himind:settings-navigate', event => {
      const aiTab = event.payload.aiTab;
      const route = settingsRoute(event.payload);
      setSettingsSection(route.section);
      setSettingsTab(route.tab);
      if (aiTab === 'mcp' || aiTab === 'services' || aiTab === 'acp') setAiConnectionsTab(aiTab);
      setPage(route.panel);
    }).then(remove => { unlisten = remove; }).catch(error => console.error('设置窗口导航监听失败', error));
    return () => unlisten?.();
  }, [isSettingsWindow]);

  useEffect(() => {
    if (!workflowCenter) return;
    for (const item of workflowCenter.runs.filter(run => run.run.status === 'waiting')) {
      const notificationKey = `${item.run.run_id}:${item.run.current_step_id}`;
      if (notifiedWorkflowApprovals.current.has(notificationKey)) continue;
      notifiedWorkflowApprovals.current.add(notificationKey);
      notify('info', `工作流“${item.workflow_name || item.workflow_id}”正在等待${item.waiting_kind === 'feedback' ? '反馈' : '审批'}，请前往“待处理”处理。`);
    }
  }, [workflowCenter]);

  // 运行结束时给一次收尾通知：任务在后台跑完（或失败）也该被看见。
  // 首次加载只做登记，避免把历史运行一次性刷成通知。
  useEffect(() => {
    if (!workflowCenter) return;
    const finished = workflowCenter.runs.filter(item => ['succeeded', 'failed', 'canceled'].includes(item.run.status));
    if (!workflowOutcomePrimed.current) {
      finished.forEach(item => notifiedWorkflowOutcomes.current.add(`${item.run.run_id}:${item.run.status}`));
      workflowOutcomePrimed.current = true;
      return;
    }
    for (const item of finished) {
      const key = `${item.run.run_id}:${item.run.status}`;
      if (notifiedWorkflowOutcomes.current.has(key)) continue;
      notifiedWorkflowOutcomes.current.add(key);
      const name = item.workflow_name || item.workflow_id;
      if (item.run.status === 'succeeded') {
        notify('success', `工作流“${name}”已完成`);
      } else if (item.run.status === 'failed') {
        notify('error', `工作流“${name}”运行失败：${item.run.error || '未提供原因'}`);
      } else {
        notify('info', `工作流“${name}”已取消`);
      }
    }
  }, [workflowCenter]);

  useEffect(() => {
    if (isSettingsWindow) return;
    const hasActiveRuns = workflowCenter?.runs.some(item => ['queued', 'running', 'waiting'].includes(item.run.status));
    if (!hasActiveRuns) return;
    // 有在跑的任务时收紧到 2.5 秒：本地调用开销很小，换来的是「能看见它在动」。
    const timer = window.setInterval(() => {
      if (document.visibilityState !== 'hidden') refreshWorkflowCenter(true).catch(console.error);
    }, 2500);
    return () => window.clearInterval(timer);
  }, [workflowCenter]);

  useEffect(() => {
    const operation = page === 'dashboard'
      ? Promise.all([refreshDashboardIdentity(), refreshMcpTargets(), refreshRemoteExecutionSettings(), refreshProjectionSyncStatus(), refreshLocalUsage()])
      : page === 'ai'
        ? Promise.all([refreshDashboardIdentity(), refreshMcpTargets(), refreshAIServices(), refreshAcpRuntimeProfiles(), refreshBuiltinAiToolContext(), refreshInferenceGateway()])
        : page === 'inbox'
          ? Promise.all([refreshApprovals(), refreshWorkflowCenter()])
        : page === 'approvals'
          ? refreshApprovals()
          // 「我的能力」三个类型页签的数据源都来自这里：插件清单、技能状态、策略、工作流。
          : page === 'installed'
            ? Promise.all([refreshExtensionDesiredState(), refreshPlugins(), refreshSkills(), refreshPluginRegistry(), refreshWorkflowCenter()])
            : page === 'workflows'
              ? refreshWorkflowCenter()
              // 市场页的三类制品来自不同数据源：插件目录、技能市场、工作流中心，
              // 外加来源快照（来源管理对话框）。缺了这一段，首次进市场只有工作流。
              : page === 'extensions'
                ? Promise.all([refreshPlugins(), refreshSkills(), refreshWorkflowCenter(), refreshExtensionSources()])
              : page === 'development'
                ? refreshDevelopment()
                : page === 'settings'
                  ? refreshSettingsPageData()
                  : Promise.resolve();
    operation.catch(console.error);
  }, [page]);

  // 各页面都不再挂手动刷新按钮，统一留一个逃生口：F5 / Ctrl+R 强制刷新当前页数据。
  // 设置窗口里的连接状态同样会随外部工具变化，所以这个逃生口也覆盖设置窗口。
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const refreshKey = event.key === 'F5' || ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'r');
      if (!refreshKey) return;
      event.preventDefault();
      // 主窗口背负全局状态，设置窗口只看自己那一块，避免刷一次带出一串无关请求。
      const tasks: Array<Promise<unknown>> = isSettingsWindow
        ? []
        : [refreshStatus(), refreshApprovals(), refreshWorkflowCenter(), refreshProjectionSyncStatus()];
      // 设置窗口里 F5 只刷当前这一块：运行日志页签和主窗口用的是同一份数据。
      if (page === 'settings' && settingsTab === 'logs') tasks.push(refreshLogs());
      if (page === 'dashboard' || page === 'ai') tasks.push(refreshDashboardIdentity(), refreshMcpTargets(), refreshRemoteExecutionSettings());
      if (page === 'dashboard') tasks.push(refreshLocalUsage({ force: true }));
      if (page === 'ai') tasks.push(refreshAIServices(), refreshAIServiceTemplates(), refreshAcpRuntimeProfiles(), refreshBuiltinAiToolContext());
      if (page === 'ai') tasks.push(refreshInferenceGateway());
      if (page === 'installed') tasks.push(refreshPlugins(), refreshSkills(), refreshExtensionDesiredState());
      if (page === 'extensions') tasks.push(refreshExtensionSurfaces(), refreshExtensionDesiredState());
      if (page === 'development') tasks.push(refreshDevelopment());
      if (page === 'settings') tasks.push(refreshWorkbenchConnections(), refreshSettingsPageData());
      if (page === 'schedules') tasks.push(refreshWorkflowCenter());
      // 定时计划只在页面内部持有计划清单，用一次性事件把这次刷新传下去。
      if (!isSettingsWindow) window.dispatchEvent(new Event('himind:refresh-page'));
      Promise.allSettled(tasks).catch(console.error);
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [page, isSettingsWindow, settingsTab]);

  useEffect(() => {
    if (isSettingsWindow) return;
    try {
      window.localStorage.setItem('himind.page', page);
    } catch {
      // Webview storage can be unavailable; navigation still works in memory.
    }
  }, [page]);

  useEffect(() => {
    if (page !== 'development' || !dashboardEnabled()) return;
    const timer = window.setInterval(() => {
      if (document.visibilityState !== 'hidden') refreshReviewProgress().catch(console.error);
    }, 60000);
    return () => window.clearInterval(timer);
  }, [page]);

  // 运行日志是目前唯一一个由「窗口被看见」驱动的轮询：定时器 5 秒一次，
  // 回到前台时补一次。focus 与 visibilitychange 会同时到达，用一个 1 秒的
  // 时间去重窗口合并，避免一次切换打出两遍同样的读取。
  const logsVisible = page === 'settings' && settingsSection === 'diagnostics' && settingsTab === 'logs';
  useEffect(() => {
    if (!logsVisible) return;
    let lastRefresh = 0;
    const refreshVisibleLogs = () => {
      if (document.visibilityState === 'hidden') return;
      const now = Date.now();
      if (now - lastRefresh < 1000) return;
      lastRefresh = now;
      refreshLogs().catch(console.error);
    };
    refreshVisibleLogs();
    const timer = window.setInterval(refreshVisibleLogs, 5000);
    window.addEventListener('focus', refreshVisibleLogs);
    document.addEventListener('visibilitychange', refreshVisibleLogs);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener('focus', refreshVisibleLogs);
      document.removeEventListener('visibilitychange', refreshVisibleLogs);
    };
  }, [logsVisible]);

  useEffect(() => {
    const snapshot = new Map<string, string>();
    const items = [
      ...pluginSubmissions.map(item => ({ id: `plugin:${item.id}`, name: item.name, state: `${item.status}:${item.release_status || ''}:${item.review_note || ''}` })),
      ...skillSubmissions.map(item => ({ id: `skill:${item.id}`, name: item.name || item.product_key, state: `${item.status}:${item.release_status || ''}:${item.review_note || ''}` })),
    ];
    for (const item of items) snapshot.set(item.id, item.state);
    const previous = reviewSnapshot.current;
    if (previous) {
      for (const item of items) {
        const before = previous.get(item.id);
        if (before && before !== item.state) notify('info', `${item.name} 的审核状态已更新`);
      }
    }
    reviewSnapshot.current = snapshot;
  }, [pluginSubmissions, skillSubmissions]);

  useEffect(() => {
    if (!dashboardEnabled() || (dashboardAuthorization?.state !== 'starting' && dashboardAuthorization?.state !== 'pending')) return;
    let stopped = false;
    const timer = window.setInterval(async () => {
      if (stopped) return;
      try {
        const progress = await agentApi.dashboardAuthorizationProgress();
        setDashboardAuthorization(progress);
        if (progress.state === 'authorized') {
          stopped = true;
          window.clearInterval(timer);
          await Promise.all([refreshDashboardIdentity(), refreshWorkbenchConnections()]);
          notify('success', progress.user_name ? `已登录工作台账号：${progress.user_name}` : '工作台账号授权成功');
        } else if (['denied', 'expired', 'failed', 'canceled'].includes(progress.state)) {
          stopped = true;
          window.clearInterval(timer);
          if (progress.state !== 'canceled') notify('error', authorizationFailure(progress));
        }
      } catch (error) {
        console.error(error);
      }
    }, 1000);
    return () => {
      stopped = true;
      window.clearInterval(timer);
    };
  }, [dashboardAuthorization?.state]);

  function dismissNotification(id: number) {
    setMessages(current => current.filter(item => item.id !== id));
  }

  function notify(kind: UiMessage['kind'], text: string) {
    const id = nextNotificationId++;
    setMessages(current => [...current.slice(-3), { id, kind, text }]);
    const duration = kind === 'error' ? 8000 : kind === 'info' ? 6000 : 4000;
    window.setTimeout(() => dismissNotification(id), duration);
  }

  async function run(action: () => Promise<unknown>, success?: string, fallback = '操作失败') {
    try {
      await action();
      if (success) notify('success', success);
    } catch (error) {
      notify('error', formatError(error, fallback));
    }
  }

  // 分发/取消分发对一部分 AI 工具（VS Code 等）只是「打开授权」：真正生效要用户
  // 在对方界面里确认，后端也会带回 status。照抄后端说法，用户才知道接着去哪一步，
  // 而不是以为在 HiMind 里点完就已经生效。
  function aiClientActionMessage(target: string, action: 'import' | 'remove', result: unknown) {
    const label = clientLabel(target);
    const status = result && typeof result === 'object' && typeof (result as { status?: unknown }).status === 'string'
      ? (result as { status: string }).status
      : '';
    if (action === 'import') {
      return status === 'authorization_opened' ? `已在 ${label} 中打开授权，确认后生效` : `已分发给 ${label}`;
    }
    if (status === 'cancellation_opened') return `已通知 ${label} 清除 HiMind 凭据`;
    if (status === 'not_imported') return `${label} 当前没有 HiMind 配置`;
    return `已取消 ${label} 的分发`;
  }

  /**
   * 设置窗口只有一个入口：面板 + 条目 + 页签。panel='ai' 时 section/tab 传空，
   * 让「AI 连接」自己决定落到哪个页签。
   */
  function openSettingsWindow(panel: SettingsWindowPanel = 'settings', section?: SettingsSection, tab?: SettingsTab, aiTab?: AiConnectionsTab) {
    void run(() => agentApi.openSettingsWindow(panel, section, tab, aiTab), undefined, '打开设置窗口失败');
  }

  /// 运行日志藏在「数据与诊断」的页签里，导出入口跟着它走，行为与原来的日志页一致。
  function exportDiagnostics() {
    void run(async () => {
      const result = await agentApi.exportDiagnostics();
      if (!result.canceled) notify('success', `诊断包已导出：${result.path || ''}`);
    }, undefined, '导出诊断包失败');
  }

  async function openBuiltinAi(project?: ExtensionProject) {
    if (project) {
      try {
        await agentApi.prepareExtensionAuthoring();
        await Promise.all([refreshPlugins(), refreshSkills()]);
      } catch (error) {
        notify('error', formatError(error, '开发工具尚未就绪，请检查来源设置'));
        return;
      }
    }
    if (project) {
      setBuiltinAiWorkspaceRequest(current => {
        if (current.target?.kind === 'project' && current.target.projectId === project.id) return current;
        return {
          target: { kind: 'project', projectId: project.id, name: project.name, path: project.workspace_path },
          revision: current.revision + 1,
        };
      });
    }
    setBuiltinAiActivated(true);
    setPage('builtin-ai');
  }

  /// 添加一个本机开发目录。选目录由后端弹系统对话框，用户取消时返回 null，
  /// 此时什么都不做（不是失败，页面不该报错）。
  async function addExtensionWorkspace() {
    const root = await agentApi.pickExtensionWorkspaceDir();
    if (!root) return;
    setExtensionWorkspaces(await agentApi.addExtensionWorkspace(root));
    await refreshDevelopment(true);
    notify('success', `已添加工作区：${workspaceLabel(root)}`);
  }

  /// 从工作区清单里移除。只动登记，目录里的文件不动，所以不需要额外确认。
  async function removeExtensionWorkspace(root: string) {
    setExtensionWorkspaces(await agentApi.removeExtensionWorkspace(root));
    await refreshDevelopment(true);
    notify('success', `已移除工作区：${workspaceLabel(root)}`);
  }

  async function invalidateBuiltinAiToolContext() {
    try {
      await agentApi.reloadBuiltinAiToolContext();
    } catch (error) {
      console.error('HiMind AI 工具上下文刷新失败', error);
    }
    setBuiltinAiWorkspaceRequest(current => ({ ...current, revision: current.revision + 1 }));
  }

  async function openExtensionWorkspaceAi(root: string) {
    const target = root.trim();
    if (!target) {
      notify('error', '请先添加并选择本地开发目录');
      return;
    }
    try {
      await agentApi.prepareExtensionAuthoring();
      await Promise.all([refreshPlugins(), refreshSkills()]);
    } catch (error) {
      notify('error', formatError(error, '开发工具尚未就绪，请检查来源设置'));
      return;
    }
    setBuiltinAiWorkspaceRequest(current => {
      if (current.target?.kind === 'extension-workspace' && current.target.path === target) return current;
      return {
        target: { kind: 'extension-workspace', name: workspaceLabel(target), path: target },
        revision: current.revision + 1,
      };
    });
    setBuiltinAiActivated(true);
    setPage('builtin-ai');
  }

  // 安装技能时可选的投放目标：本机探测到的其他 AI 工具。himind-ai 由后端强制保留，不参与选择。
  const installTargets = useMemo(() => skillClientDescriptors(skillStatus, mcpTargets, clientMatrix)
    .filter(client => client.id !== 'himind-ai')
    .filter(client => client.detected || Boolean(targetForSkillClient(mcpTargets, client.id)?.detected))
    .map(client => ({ id: client.id, name: client.name, detected: client.detected })), [skillStatus, mcpTargets, clientMatrix]);

  async function runSkillOperation(key: string, action: () => Promise<string>, fallback: string) {
    if (skillOperation) return;
    setSkillOperation(key);
    try {
      const message = await action();
      // 空消息表示"这次操作不需要提示"（例如用户取消了目录选择）。
      if (message) notify('success', message);
    } catch (error) {
      notify('error', formatError(error, fallback));
    } finally {
      setSkillOperation(null);
    }
  }

  async function runDevelopmentOperation(key: string, action: () => Promise<void>, success: string, fallback: string) {
    if (developmentOperation) return;
    setDevelopmentOperation(key);
    try {
      await action();
      notify('success', success);
    } catch (error) {
      notify('error', formatError(error, fallback));
    } finally {
      setDevelopmentOperation(null);
    }
  }

  const availablePlugins = useMemo(() => {
    const merged = new Map(pluginCatalog.map(item => [item.plugin_id, item]));
    for (const item of pluginRegistry?.items || []) {
      if (!item.development && merged.has(item.id)) continue;
      merged.set(item.id, {
        plugin_id: item.id,
        name: item.name || item.id,
        description: item.description || (item.development ? '本机开发插件' : '本机插件'),
        author_name: item.author_name,
        categories: [],
        governance: item.governance || 'optional',
        version: item.version || '0.0.0',
        release_notes: '',
        min_agent_version: item.min_agent_version || '',
        artifact_id: '',
        file_size: item.entry_size || 0,
        sha256: '',
        source: item.development ? 'development' : 'system',
        capability_ids: (item.capabilities || []).map(capability => capability.id),
        permissions: item.permissions || [],
        view_count: item.views?.length || 0,
      });
    }
    return Array.from(merged.values());
  }, [pluginCatalog, pluginRegistry]);

  const quickPluginViews = useMemo<PluginQuickAccessView[]>(() => {
    const views: PluginQuickAccessView[] = [];
    for (const plugin of pluginRegistry?.items || []) {
      if (!plugin.enabled || plugin.circuit_open) continue;
      for (const view of plugin.views || []) {
        // Missing location is treated as the historical/default navigation
        // location so older registry payloads remain discoverable.
        if ((view.location || 'plugin_navigation') !== 'plugin_navigation' || view.quick_access === false) continue;
        const title = view.title.trim();
        if (!title) continue;
        // A view title is usually the clearest compact label. The plugin name
        // is only a last resort for malformed/legacy registry entries; the
        // full title remains available in the tooltip.
        const shortTitle = view.short_title?.trim() || title || plugin.name?.trim() || plugin.id;
        views.push({
          plugin_id: plugin.id,
          plugin_name: plugin.name?.trim() || plugin.id,
          view_id: view.id,
          title,
          short_title: shortTitle,
          icon: view.icon?.trim() || 'app-window',
          order: view.order ?? 0,
        });
      }
    }
    return views.sort((left, right) =>
      left.order - right.order
      || left.short_title.localeCompare(right.short_title, 'zh-CN')
      || left.plugin_name.localeCompare(right.plugin_name, 'zh-CN')
      || left.view_id.localeCompare(right.view_id),
    );
  }, [pluginRegistry]);

  async function startDashboardAuthorization() {
    if (aiOperation) return;
    setAiOperation('identity');
    try {
      setDashboardAuthorization(await agentApi.startDashboardAuthorization());
    } catch (error) {
      notify('error', friendlyConnectionError(error, '无法打开工作台登录授权，请检查网络后重试。'));
    } finally {
      setAiOperation(null);
    }
  }

  async function cancelDashboardAuthorization() {
    try {
      setDashboardAuthorization(await agentApi.cancelDashboardAuthorization());
    } catch (error) {
      notify('error', friendlyConnectionError(error, '暂时无法取消登录授权，请稍后重试。'));
    }
  }

  async function revokeDashboardAuthorization() {
    if (aiOperation) return;
    if (!await confirm({
      title: '取消 HiMind 账号授权？',
        description: '取消后不再接收工作台任务与运行记录；本机能力不受影响，设备注册需在组织侧解除。',
      confirmText: '取消授权',
    })) return;
    setAiOperation('identity');
    try {
      await agentApi.revokeDashboardAuthorization();
      setDashboardAuthorization(null);
      await Promise.all([refreshDashboardIdentity(), refreshWorkbenchConnections()]);
      notify('success', '已取消工作台账号授权');
    } catch (error) {
      notify('error', friendlyConnectionError(error, '暂时无法取消工作台账号授权，请稍后重试。'));
    } finally {
      setAiOperation(null);
    }
  }

  /** 切换后本机地址、凭据和会话都变了，凡是跟着连接走的数据都必须重读。 */
  async function refreshAfterWorkbenchChange() {
    await Promise.all([
      refreshDashboardIdentity(),
      refreshStatus(),
      refreshLogs(),
    ]);
  }

  async function performWorkbenchSwitch(connection: WorkbenchConnection, force = false): Promise<boolean> {
    setWorkbenchBusyId(`switch:${connection.id}`);
    try {
      setWorkbenchConnections(await agentApi.switchWorkbenchConnection(connection.id, force));
      await refreshAfterWorkbenchChange();
      notify('success', `已切换到 ${connection.display_name || connection.api_base}`);
      return true;
    } catch (error) {
      const detail = errorDetail(error);
      // 后端只在有任务在跑、且没有强制切换时用这个前缀拒绝，前端负责把决定权交回用户。
      if (!force && detail.startsWith('WORKBENCH_BUSY')) {
        const reason = detail.replace(/^WORKBENCH_BUSY:\s*/, '');
        if (await confirm({
          title: '仍要切换工作台？',
          description: `${reason}。切换会中断这个任务。`,
          confirmText: '仍然切换',
        })) {
          return performWorkbenchSwitch(connection, true);
        }
        return false;
      }
      notify('error', formatError(error, '切换工作台失败'));
      return false;
    } finally {
      setWorkbenchBusyId(current => (current === `switch:${connection.id}` ? '' : current));
    }
  }

  function switchWorkbenchConnection(connection: WorkbenchConnection) {
    if (workbenchBusyId || aiOperation) return;
    void performWorkbenchSwitch(connection);
  }

  /** 未激活的连接要先切换再授权，否则授权会落到当前那条连接上。 */
  async function authorizeWorkbenchConnection(connection: WorkbenchConnection) {
    if (workbenchBusyId || aiOperation) return;
    if (!connection.active) {
      const switched = await performWorkbenchSwitch(connection);
      if (!switched) return;
    }
    await startDashboardAuthorization();
  }

  async function addWorkbenchConnection(draft: WorkbenchConnectionDraft) {
    setWorkbenchBusyId('add');
    try {
      const next = await agentApi.addWorkbenchConnection(draft.apiBase, draft.displayName, draft.purpose);
      setWorkbenchConnections(next);
      const token = draft.enrollmentToken.trim();
      if (!token) {
        notify('success', '已添加工作台，登记后即可接收任务');
        return;
      }
      const normalize = (value: string) => value.trim().replace(/\/+$/, '').toLowerCase();
      const added = next.connections.find(connection => normalize(connection.api_base) === normalize(draft.apiBase));
      if (!added) {
        notify('success', '已添加工作台，请在列表中完成登记');
        return;
      }
      setWorkbenchBusyId(`enroll:${added.id}`);
      try {
        setWorkbenchConnections(await agentApi.enrollWorkbenchConnection(added.id, token));
      } catch (error) {
        throw new UserFacingError(enrollFailureText(error));
      }
      await refreshAfterWorkbenchChange();
      notify('success', '工作台登记完成，请登录账号');
    } catch (error) {
      throw error instanceof UserFacingError ? error : new Error(formatError(error, '连接工作台失败'));
    } finally {
      setWorkbenchBusyId('');
    }
  }

  async function enrollWorkbenchConnection(id: string, enrollmentToken: string) {
    setWorkbenchBusyId(`enroll:${id}`);
    try {
      setWorkbenchConnections(await agentApi.enrollWorkbenchConnection(id, enrollmentToken));
      await refreshAfterWorkbenchChange();
      notify('success', '工作台登记完成，请登录账号');
    } catch (error) {
      throw new UserFacingError(enrollFailureText(error));
    } finally {
      setWorkbenchBusyId('');
    }
  }

  async function renameWorkbenchConnection(id: string, displayName: string, purpose: string) {
    setWorkbenchBusyId(`rename:${id}`);
    try {
      setWorkbenchConnections(await agentApi.renameWorkbenchConnection(id, displayName, purpose));
    } catch (error) {
      throw new Error(formatError(error, '保存工作台名称失败'));
    } finally {
      setWorkbenchBusyId('');
    }
  }

  async function removeWorkbenchConnection(connection: WorkbenchConnection) {
    if (workbenchBusyId) return;
    if (!await confirm({
      title: `移除工作台「${connection.display_name || connection.api_base}」？`,
      description: '只删除这台电脑上的连接记录，工作台侧不受影响。',
      confirmText: '移除',
    })) return;
    setWorkbenchBusyId(`remove:${connection.id}`);
    try {
      setWorkbenchConnections(await agentApi.removeWorkbenchConnection(connection.id));
      notify('success', '已移除工作台连接');
    } catch (error) {
      notify('error', formatError(error, '移除工作台失败'));
    } finally {
      setWorkbenchBusyId('');
    }
  }

  async function probeWorkbenchConnection(apiBase: string): Promise<WorkbenchProbe> {
    return agentApi.probeWorkbenchConnection(apiBase);
  }

  async function applyMcpTarget(targetId: string, resetInvalid = false) {
    if (aiOperation) return;
    setAiOperation(`target:${targetId}`);
    try {
      const result = await agentApi.applyMcpRegistration(targetId, resetInvalid);
      await refreshMcpTargets();
      notify('success', result.changed ? `${result.target.name} 的连接已更新` : `${result.target.name} 已连接`);
    } catch (error) {
      notify('error', friendlyConnectionError(error, '连接失败，请关闭对应 AI 工具后重试。'));
    } finally {
      setAiOperation(null);
    }
  }

  async function removeMcpTarget(targetId: string) {
    if (aiOperation) return;
    setAiOperation(`remove:${targetId}`);
    try {
      const result = await agentApi.removeMcpRegistration(targetId);
      await refreshMcpTargets();
      notify('success', result.changed ? `已断开 ${result.target.name}` : `${result.target.name} 当前未连接`);
    } catch (error) {
      notify('error', friendlyConnectionError(error, '断开连接失败，请关闭对应 AI 工具后重试。'));
    } finally {
      setAiOperation(null);
    }
  }

  async function applyAllMcpTargets() {
    if (aiOperation) return;
    setAiOperation('apply-all');
    try {
      const result = await agentApi.applyAllMcpRegistrations(true, false);
      await refreshMcpTargets();
      if (result.failures.length) {
        notify('error', `已连接 ${result.results.length} 个 AI 工具，${result.failures.length} 个需要单独处理`);
      } else {
        notify('success', result.results.length ? `已连接 ${result.results.length} 个 AI 工具` : '已发现的 AI 工具均已连接');
      }
    } catch (error) {
      notify('error', friendlyConnectionError(error, '连接 AI 工具失败。'));
    } finally {
      setAiOperation(null);
    }
  }

  async function removeAllMcpTargets() {
    if (aiOperation) return;
    setAiOperation('remove-all');
    try {
      const result = await agentApi.removeAllMcpRegistrations(true);
      await refreshMcpTargets();
      if (result.failures.length) {
        notify('error', `已断开 ${result.results.length} 个连接，${result.failures.length} 个需处理`);
      } else {
        notify('success', result.results.length ? `已断开 ${result.results.length} 个连接` : '当前没有可断开的连接');
      }
    } catch (error) {
      notify('error', friendlyConnectionError(error, '断开连接失败。'));
    } finally {
      setAiOperation(null);
    }
  }

  async function testMcpConnection() {
    if (aiOperation) return;
    setAiOperation('test');
    setMcpTestResult(null);
    try {
      const result = await agentApi.testMcpConnection();
      setMcpTestResult(result);
       notify('success', '本机连接正常');
    } catch (error) {
       notify('error', friendlyConnectionError(error, '本机连接检查失败，请重新启动 HiMind Agent。'));
    } finally {
      setAiOperation(null);
    }
  }

  async function runUpdateOperation(action: () => Promise<AgentUpdateStatus>, success?: (status: AgentUpdateStatus) => string) {
    if (updateBusy) return;
    setUpdateBusy(true);
    try {
      const result = await action();
      setUpdateStatus(result);
      if (success) notify('success', success(result));
    } catch (error) {
      try { await refreshUpdateStatus(); } catch { /* preserve update error */ }
      notify('error', formatError(error, '软件更新操作失败'));
    } finally {
      setUpdateBusy(false);
    }
  }

  async function cancelUpdateDownload() {
    try {
      const result = await agentApi.cancelUpdateDownload();
      setUpdateStatus(result);
      notify('info', '正在取消更新下载');
    } catch (error) {
      notify('error', formatError(error, '取消更新下载失败'));
    }
  }

  function openLoginModal() {
    setLoginUsername(current => current || loginState?.account || '');
    setLoginPassword('');
    setLoginModalOpen(true);
  }

  // Keep the embedded AI page mounted while navigating elsewhere. Destroying
  // the iframe on every navigation loses the runtime's browser state and
  // forces a full session reload when the user comes back.
  const builtinAiContent = <BuiltinAiPage
      independentMode={status?.mode === 'independent' || status?.dashboard_enabled === false}
      identity={dashboardIdentity}
      authorization={dashboardAuthorization}
      authorizationBusy={aiOperation === 'identity'}
      onStartAuthorization={startDashboardAuthorization}
      onCancelAuthorization={cancelDashboardAuthorization}
      onOpenAuthorization={() => run(agentApi.openDashboardAuthorizationPage)}
      onOpenSettings={() => openSettingsWindow('settings')}
      onOpenAiConnections={() => openSettingsWindow('ai', undefined, undefined, 'services')}
      onOpenCapabilities={() => openInstalled('skill')}
      onOpenMcpTools={openMcpTools}
      onToolContextChanged={() => { void refreshBuiltinAiToolContext(); void invalidateBuiltinAiToolContext(); }}
      skillCount={builtinAiToolContext?.skills ?? null}
      workspaceTarget={builtinAiWorkspaceRequest.target}
      workspaceRequestRevision={builtinAiWorkspaceRequest.revision}
    />;

  const workflowApprovalCount = workflowCenter?.runs.filter(item => {
    if (item.run.status !== 'waiting') return false;
    const waitingKind = item.interaction_request?.kind || item.waiting_kind;
    return waitingKind !== 'external_wait';
  }).length || 0;
  // 顶栏状态入口、侧栏徽标、常驻运行条共用同一份口径：排队、运行、等待处理的运行都算「在跑」。
  const activeRuns = (workflowCenter?.runs || []).filter(item => ['queued', 'running', 'waiting'].includes(item.run.status));
  const activeRunCount = activeRuns.length;
  // 常驻运行条只需要一条代表：第一条在跑的运行 + 同时在跑的总数。
  const primaryRun = activeRuns[0] || null;
  const activeRunSummary: ActiveRunSummary | null = primaryRun ? {
    title: `工作流 · ${primaryRun.workflow_name || primaryRun.workflow_id || '本机运行'}`,
    runId: primaryRun.run.run_id,
    // 等待交互的运行要点名「等待你的处理」，让常驻条和「待处理」对得上。
    stage: primaryRun.waiting_kind && primaryRun.waiting_kind !== 'external_wait'
      ? '等待你的处理'
      : primaryRun.current_step_title || primaryRun.business_stage || '运行中',
    startedAt: primaryRun.run.created_at,
    count: activeRuns.length,
  } : null;

  /// 「我的能力」的类型页签：切换时记住选择，跨页跳转（市场"管理"、快捷工具"更多"）也落到指定页签。
  function openInstalled(kind?: InstalledKind) {
    if (kind) setInstalledKind(kind);
    setPage('installed');
  }
  useEffect(() => {
    if (page !== 'installed') return;
    try { window.localStorage.setItem('himind-agent.installed-kind', installedKind); } catch { /* storage is optional */ }
  }, [page, installedKind]);

  /// 市场里的「MCP 工具」页签是获得工具的入口；会话对话框和「我的能力」都只是把用户送过来。
  function openMcpTools() {
    setOpenMcpRequest(current => current + 1);
    setPage('extensions');
  }

  function navigate(target: NavigationTarget) {
    const nextPage = typeof target === 'string' ? target : target.page;
    if (typeof target !== 'string' && target.kind) setInstalledKind(target.kind);
    // 从侧栏/「查看」菜单进入 AI 对话时必须同时点亮激活标记：页面内容由
    // builtinAiActivated 门控，只有 openBuiltinAi() 系列入口会设置它。
    // 漏掉这一步，当应用上次停在别的页面（标记初值为 false）时，从菜单进来
    // 会渲染成整片空白——连页面自带的工具栏都没有，只有外层导航还在。
    if (nextPage === 'builtin-ai') setBuiltinAiActivated(true);
    if (!isSettingsWindow && nextPage === 'ai') {
      setAiConnectionsTab('mcp');
      openSettingsWindow('ai');
      return;
    }
    if (!isSettingsWindow && nextPage === 'settings') {
      setSettingsSection('general');
      setSettingsTab(null);
      openSettingsWindow('settings', 'general');
      return;
    }
    if (!isSettingsWindow && nextPage === 'logs') {
      openSettingsWindow('settings', 'diagnostics', 'logs');
      return;
    }
    if (nextPage === 'ai') setAiConnectionsTab('mcp');
    if (nextPage === 'settings') { setSettingsSection('general'); setSettingsTab(null); }
    if (nextPage === 'logs') { setSettingsSection('diagnostics'); setSettingsTab('logs'); }
    if (typeof target !== 'string' && target.runId) setWorkflowRunTarget(target.runId);
    else if (nextPage === 'workflows') setWorkflowRunTarget('');
    if (typeof target !== 'string' && target.workflowId) setSchedulePresetTarget(target.workflowId);
    else if (nextPage !== 'schedules') setSchedulePresetTarget('');
    setPage(nextPage);
  }
  const content = (() => {
    if (page === 'builtin-ai') return null;
    if (page === 'dashboard') return <DashboardPage
      status={status}
      projectionSyncStatus={projectionSyncStatus}
      approvals={approvals}
      remoteExecutionSettings={remoteExecutionSettings}
      mcpTargets={mcpTargets}
      localUsage={localUsage}
      localUsageRange={localUsageRange}
      localUsageBusy={localUsageBusy}
      inferenceGateway={inferenceGateway}
      identity={dashboardIdentity}
      authorization={dashboardAuthorization}
      identityBusy={aiOperation === 'identity'}
      updateStatus={updateStatus}
      updateBusy={updateBusy}
      projectionRequeueBusy={projectionRequeueBusy}
      onOpenDashboard={() => run(agentApi.openDashboard)}
      onRequeueProjectionDeadLetters={requeueProjectionDeadLetters}
      onStartAuthorization={startDashboardAuthorization}
      onCancelAuthorization={cancelDashboardAuthorization}
      onOpenAuthorization={() => run(agentApi.openDashboardAuthorizationPage)}
      onRefreshIdentity={() => run(refreshDashboardIdentity)}
      onRevokeAuthorization={revokeDashboardAuthorization}
      onLocalUsageRangeChange={changeLocalUsageRange}
      onRefreshLocalUsage={() => void refreshLocalUsage({ force: true })}
      onBindCodexToGateway={() => void bindCodexToGateway()}
      bindingModeBusy={bindingModeBusy}
      onCheckUpdate={() => runUpdateOperation(agentApi.checkUpdate, result => result.available_version ? `发现新版本 v${result.available_version}` : '当前已是最新版本')}
      onDownloadUpdate={() => runUpdateOperation(agentApi.downloadUpdate, result => `v${result.available_version} 更新已下载`)}
      onInstallUpdate={() => runUpdateOperation(agentApi.installUpdate)}
    />;
    // 分发模型服务只写这些 AI 工具自身的服务配置，不动 HiMind 的 MCP 注册，
    // 所以分发/取消分发后只回读模型服务列表：少跑一趟要 1s 的 MCP 目标探测，
    // 按钮不必为此多转一圈。
    if (page === 'ai') return <AiConnectionsPage
      initialTab={aiConnectionsTab}
      identity={dashboardIdentity}
      dashboardEnabled={dashboardEnabled()}
      testResult={mcpTestResult}
      busyAction={aiOperation}
      aiServices={aiServices}
      aiServiceTemplates={aiServiceTemplates}
      gatewayStatus={inferenceGateway}
      onSetBindingMode={setClientBindingMode}
      gatewayBusy={gatewayBusy}
      onRestartGateway={() => void restartGateway()}
      onStopGateway={() => void stopGatewayAndUnbind()}
      acpProfiles={acpRuntimeProfiles}
      onOpenAccount={() => setPage('dashboard')}
      targets={mcpTargets}
      onRefresh={() => run(async () => { await Promise.all([refreshDashboardIdentity(), refreshMcpTargets(), refreshAIServices(), refreshAIServiceTemplates(), refreshAcpRuntimeProfiles()]); })}
      onApplyTarget={applyMcpTarget}
      onApplyAll={applyAllMcpTargets}
      onRemoveAll={removeAllMcpTargets}
      onRemoveTarget={removeMcpTarget}
      onOpenDirectory={(path) => run(() => agentApi.openFolder(path))}
      onTest={testMcpConnection}
      onSaveAIService={async (input) => { try { await agentApi.saveAIService(input); await refreshAIServices(); notify('success', '模型服务已保存'); } catch (error) { notify('error', formatError(error, '保存模型服务失败')); throw error; } }}
      onSetActiveAIService={(id) => run(async () => { await agentApi.setActiveAIService(id); await refreshAIServices(); }, id ? 'HiMind AI 对话已改用所选服务' : 'HiMind AI 对话已恢复默认服务', '切换 HiMind AI 对话服务失败')}
      onRemoveAIService={(id) => run(async () => { await agentApi.removeAIService(id); await refreshAIServices(); }, '模型服务已删除', '删除模型服务失败')}
      onFetchModels={(input) => agentApi.fetchAIServiceModels(input).then((result) => result.models)}
      onFetchSavedModels={(id, baseUrl) => agentApi.fetchSavedAIServiceModels(id, baseUrl).then((result) => result.models)}
      onImportAIClient={async (target, service, replace) => {
        try {
          const result = await agentApi.importAIClient(target, service, replace);
          await refreshAIServices();
          notify('success', aiClientActionMessage(target, 'import', result));
        } catch (error) {
          notify('error', formatError(error, '分发到 AI 工具失败'));
        }
      }}
      onRemoveAIClient={async (target) => {
        try {
          const result = await agentApi.removeAIClient(target);
          await refreshAIServices();
          notify('success', aiClientActionMessage(target, 'remove', result));
        } catch (error) {
          notify('error', formatError(error, '取消分发失败'));
        }
      }}
      onSaveAcpProfile={async (input) => { try { await agentApi.saveAcpRuntimeProfile(input); await refreshAcpRuntimeProfiles(); notify('success', '运行环境已保存'); } catch (error) { notify('error', formatError(error, '保存运行环境失败')); throw error; } }}
      onSetAcpProfileEnabled={(providerId, enabled) => run(async () => { await agentApi.setAcpRuntimeProfileEnabled(providerId, enabled); await refreshAcpRuntimeProfiles(); }, enabled ? '运行环境已启用' : '运行环境已停用', '更新运行环境状态失败')}
      onRemoveAcpProfile={(providerId) => run(async () => { await agentApi.removeAcpRuntimeProfile(providerId); await refreshAcpRuntimeProfiles(); }, '运行环境已删除', '删除运行环境失败')}
    />;
    if (page === 'inbox') return <InboxPage
      approvals={approvals}
      workflowRuns={(workflowCenter?.runs || []).filter(item => item.run.status === 'waiting')}
      onRefresh={() => { void run(async () => { await Promise.all([refreshApprovals(), refreshWorkflowCenter()]); }); }}
      onRespond={(id, approved) => run(async () => { await agentApi.respondApproval(id, approved); await Promise.all([refreshApprovals(), refreshWorkflowCenter(false, true), refreshStatus()]); }, undefined, '审批处理失败')}
      onOpenWorkflowRun={(runId) => navigate({ page: 'workflows', runId })}
      onOpenApprovalHistory={() => navigate('approvals')}
    />;
    if (page === 'approvals') return <ApprovalsPage independentMode={status?.mode === 'independent' || status?.dashboard_enabled === false} approvals={approvals} history={approvalHistory} onRefresh={() => run(refreshApprovals)} onRespond={(id, approved) => run(async () => { await agentApi.respondApproval(id, approved); await refreshApprovals(); await refreshStatus(); }, undefined, '审批处理失败')} onOpenSettings={() => openSettingsWindow('settings', 'approval')} />;
    if (page === 'tasks') return <TaskCenterPage
      dashboardEnabled={dashboardEnabled()}
      currentTask={status?.current_task || null}
      onLoadTaskHistory={agentApi.taskHistory}
      onLoadLocalActivity={agentApi.localActivity}
      onOpenDashboard={() => run(agentApi.openDashboard)}
      onOpenWorkflowRun={(runId) => navigate({ page: 'workflows', runId })}
    />;
    if (page === 'extensions') return <ExtensionsPage
      loading={pluginsLoading || workflowLoading || extensionSourcesLoading}
      errors={[
        pluginCatalogError ? { module: '插件', message: pluginCatalogError } : null,
        instructionPackError ? { module: '项目规则', message: instructionPackError } : null,
        skillMarketError ? { module: '技能市场', message: skillMarketError } : null,
        workflowError ? { module: '工作流', message: workflowError } : null,
        skillError ? { module: '技能', message: skillError } : null,
        extensionSourcesError ? { module: '来源', message: extensionSourcesError } : null,
      ].filter((item): item is MarketLoadError => item !== null)}
      plugins={pluginCatalog}
      instructionPacks={instructionPacks}
      experts={experts}
      expertCatalog={expertCatalog}
      activeExpert={activeExpert ? `${activeExpert.expert_id}@${activeExpert.version}` : ''}
      onRefreshExperts={refreshExperts}
      onActivateExpert={async (id, version) => {
        try {
          const activation = await agentApi.activateExpert(id, version);
          setActiveExpert({ expert_id: activation.expert_id, version: activation.version });
          notify('success', '专家已切换');
        } catch (error) { notify('error', formatError(error, '切换专家失败')); throw error; }
      }}
      onNotify={(message, tone = 'success') => notify(tone, message)}
      onInstallMarketExpert={async item => { try { await agentApi.installExpertMarket(item.expert_id, item.version, item.artifact_id, item.sha256); await refreshExperts(); notify('success', '专家已安装'); } catch (error) { notify('error', formatError(error, '安装专家失败')); throw error; } }}
      installedPlugins={pluginRegistry?.items || []}
      skills={organizationSkills}
      installedSkills={skillStatus?.items || []}
      workflows={workflowCenter?.catalog || []}
      installedWorkflows={workflowCenter?.workflows || []}
      units={extensionSourceSnapshot?.units || []}
      busyUnit={installingUnitKey}
      workspace={extensionWorkspace}
      extensionSources={extensionSources}
      extensionSourceSnapshot={extensionSourceSnapshot}
      extensionSourcesLoading={extensionSourcesLoading}
      extensionSourcesError={extensionSourcesError}
      onRefresh={() => run(async () => { await Promise.all([refreshPlugins(), refreshSkills(), refreshWorkflowCenter(), refreshExtensionSources()]); })}
      onRefreshSources={refreshExtensionSources}
      onAddSource={addExtensionSource}
      onUpdateSourceConfig={updateExtensionSource}
      onRemoveSource={removeExtensionSource}
      onSetUnitAcquisition={setExtensionUnitAcquisition}
      onDevelopWorkspace={(root: string) => { void switchExtensionWorkspace(root).then(() => setPage('development')).catch(error => notify('error', formatError(error, '打开扩展开发失败'))); }}
      openSourcesRequest={extensionSourcesRequest}
      onSourcesRequestHandled={() => setExtensionSourcesRequest(0)}
      mcp={mcp}
      openMcpRequest={openMcpRequest}
      onMcpRequestHandled={() => setOpenMcpRequest(0)}
      onManageMcp={() => openInstalled('mcp')}
      onInstallUnit={installExtensionUnit}
      onOpenKind={(kind) => openInstalled(kind === 'plugin' ? 'plugin' : kind === 'skill' ? 'skill' : kind === 'expert' ? 'expert' : 'workflow')}
      onPlanPlugin={agentApi.planPluginInstall}
      onInstallPlugin={async (pluginId, version, source, artifactId, sha256) => { try { await agentApi.installPlugin(pluginId, version, source, artifactId, sha256); await refreshExtensionSurfaces(); await invalidateBuiltinAiToolContext(); notify('success', `已安装插件${version ? ` v${version}` : ''}`); } catch (error) { notify('error', formatError(error, '安装插件失败')); } }}
      onPlanSkill={agentApi.planOrganizationSkillInstall}
      installTargets={installTargets}
      onPickSkillLocation={agentApi.pickSkillLocation}
      onInstallSkill={async (skillId, version, optionalPluginIds, source, artifactId, sha256, clients, location) => { try { const result = await agentApi.installOrganizationSkill(skillId, version, optionalPluginIds, source, artifactId, sha256, clients, location); await refreshExtensionSurfaces(); await invalidateBuiltinAiToolContext(); notify('success', `已安装 ${result.record.manifest.name} v${result.record.manifest.version}`); } catch (error) { notify('error', formatError(error, '安装技能失败')); } }}
      onLoadPluginVersions={agentApi.pluginVersions}
      onLoadSkillVersions={agentApi.skillVersions}
      onLoadWorkflowVersions={agentApi.workflowVersions}
      onInstallWorkflow={async (workflowId, version, source, artifactId, sha256) => { try { await agentApi.installWorkflowCatalogItem(workflowId, version, source, artifactId, sha256); await refreshWorkflowCenter(false, true); notify('success', `已安装工作流 v${version}`); } catch (error) { notify('error', formatError(error, '安装工作流失败')); } }}
      onInstallInstructionPack={async (id, version, artifactId, sha256) => {
        try {
          await agentApi.installInstructionPackMarket(id, version, artifactId, sha256);
          notify('success', '已导入项目规则草稿，请前往扩展开发的规则库完成确认和发布');
        } catch (error) {
          notify('error', formatError(error, '导入项目规则失败'));
        }
      }}
      onBatchUpdateFinished={async () => { await refreshExtensionSurfaces(); await refreshExtensionDesiredState(); await invalidateBuiltinAiToolContext(); }}
    />;
    if (page === 'workflows') return <WorkflowsPage
      snapshot={workflowCenter}
      runtimeProfiles={acpRuntimeProfiles?.profiles ?? []}
      mode="runs"
      onOpenCapabilities={() => openInstalled('workflow')}
      initialRunId={workflowRunTarget}
      loading={workflowLoading}
      error={workflowError}
      onRefresh={() => { void refreshWorkflowCenter(); }}
      onLoadRun={loadWorkflowRun}
      onVerify={verifyWorkflowRun}
      onRevealArtifact={(runId, artifactId) => run(() => agentApi.revealWorkflowArtifact(runId, artifactId), undefined, '无法打开输出文件位置')}
      onApprove={approveWorkflowRun}
      onReject={rejectWorkflowRun}
      onResume={resumeWorkflowRun}
      onCancel={cancelWorkflowRun}
      onStart={startWorkflowRun}
      onPreflight={agentApi.preflightWorkflowRun}
      onSaveCredentialFile={async (connectorId, handle) => { try { const result = await agentApi.saveConnectorFileCredential(connectorId, handle); if (!result.cancelled) notify('success', '凭据文件已保存'); return !result.cancelled; } catch (error) { notify('error', formatError(error, '保存凭据文件失败')); throw error; } }}
      onSaveCredentialSecret={async (connectorId, handle, secret) => { try { await agentApi.saveConnectorSecretCredential(connectorId, handle, secret); notify('success', '连接密钥已保存'); } catch (error) { notify('error', formatError(error, '保存连接密钥失败')); throw error; } }}
      onInstallLocal={async () => { try { const picked = await agentApi.pickWorkflowArchive(); if (!picked.path) return; await agentApi.installLocalWorkflowArchive(picked.path); await refreshWorkflowCenter(false, true); notify('success', '本地工作流已安装'); } catch (error) { notify('error', formatError(error, '安装本地工作流失败')); } }}
      onSetEnabled={(packageId, enabled) => run(async () => { await agentApi.setWorkflowEnabled(packageId, enabled); await refreshWorkflowCenter(false, true); }, enabled ? '工作流已启用' : '工作流已停用', '更新工作流状态失败')}
      onRemove={(packageId) => run(async () => { await agentApi.removeWorkflow(packageId); await refreshWorkflowCenter(false, true); }, '工作流已移除', '移除工作流失败')}
      onPickDirectory={async () => { const result = await agentApi.pickWorkspaceDirectory(); return result.path || null; }}
      onOpenExtensions={() => setPage('extensions')}
      onScheduleWorkflow={(workflowId) => { setSchedulePresetTarget(workflowId); setPage('schedules'); }}
      onLoadPresets={(workflowId) => agentApi.workflowPresets(workflowId)}
      onSavePreset={(input) => run(async () => { await agentApi.setWorkflowPreset(input); }, '启动方案已保存', '保存启动方案失败')}
      onDeletePreset={(id) => run(async () => { await agentApi.deleteWorkflowPreset(id); }, '启动方案已删除', '删除启动方案失败')}
    />;
    if (page === 'schedules') return <SchedulesPage
      snapshot={workflowCenter}
      skills={(skillCatalog?.items || []).map(item => ({ id: item.record.manifest.id, name: item.record.manifest.name, version: item.record.manifest.version }))}
      onRefreshWorkflows={() => { void refreshWorkflowCenter(); }}
      onLoadSchedules={agentApi.schedules}
      // 计划页要拿到保存后的句柄（名称留空时后端按目标生成），并且要能就地显示失败原因，
      // 所以这里不用 run() 兜住异常，而是抛回去给表单。
      onSaveSchedule={async input => {
        try {
          const result = await agentApi.setSchedule(input);
          notify('success', '定时计划已保存');
          return result.schedule?.id || '';
        } catch (error) {
          const message = formatError(error, '保存定时计划失败');
          notify('error', message);
          throw new Error(message);
        }
      }}
      // 静默模式用于改名时清理旧句柄：用户只点了一次保存，不该再收到一条「已删除」提示。
      onDeleteSchedule={async (id, options) => {
        try {
          await agentApi.deleteSchedule(id);
          if (!options?.silent) notify('success', '定时计划已删除');
        } catch (error) {
          const message = formatError(error, '删除定时计划失败');
          if (!options?.silent) notify('error', message);
          throw new Error(message);
        }
      }}
      onRunTarget={async (kind, targetId, input) => {
        if (kind !== 'workflow') throw new Error(`unsupported target kind: ${kind}`);
        const preflight = await agentApi.preflightWorkflowRun(targetId, input);
        if (!preflight.ready) throw new Error(preflight.blockers[0] || '启动前检查未通过');
        const started = await startWorkflowRun(targetId, input);
        setWorkflowRunTarget(started.run_id);
        setPage('workflows');
      }}
      onRunSkill={(skillId, input) => run(async () => { await agentApi.runSkill(skillId, input); }, '技能已开始运行', '运行技能失败')}
      onLoadWorkflowPresets={(workflowId) => agentApi.workflowPresets(workflowId)}
      onPreflightWorkflow={agentApi.preflightWorkflowRun}
      onLoadSkillRuns={(limit) => agentApi.skillRuns(limit)}
      onRevealSkillRun={(runId) => run(() => agentApi.revealSkillRun(runId), undefined, '定位技能结果失败')}
      onPickDirectory={async () => { const result = await agentApi.pickWorkspaceDirectory(); return result.path || null; }}
      presetTargetId={schedulePresetTarget}
      onOpenExtensions={() => setPage('extensions')}
    />;
    // 「我的能力」：自己装的 + 组织配发的。类型是页内页签，页面标题归容器，
    // 所以三个类型视图都按"只出内容与动作区"的方式嵌进来。
    if (page === 'installed') return <InstalledPage
      kind={installedKind}
      dashboardEnabled={dashboardEnabled()}
      onSelectKind={setInstalledKind}
      counts={{
        plugin: userInstalledPlugins(pluginRegistry?.items || [], extensionDesiredState, dashboardEnabled()).length,
        skill: installedSkills(skillStatus, extensionDesiredState, organizationSkills, dashboardEnabled()).length,
        workflow: (workflowCenter?.workflows || []).length,
        expert: experts.length,
        instruction: instructionDrafts.length,
        mcp: mcp.servers.length,
        policy: managedItems(extensionDesiredState, pluginRegistry, skillStatus).length,
      }}
    >
      {installedKind === 'instruction' ? <InstructionProjectionPanel embedded clientsTab={false} workspaceRoot="" onLibraryChanged={() => { void refreshDevelopment(); }} onMaterializeToWorkspace={async (draft) => { await agentApi.materializeInstructionProject(extensionWorkspace.root, draft.manifest.id, draft.manifest.version); await Promise.all([refreshDevelopment(), refreshExtensionProjects()]); }} /> : installedKind === 'expert' ? <ExpertStudioPanel experts={experts} activeExpert={activeExpert ? `${activeExpert.expert_id}@${activeExpert.version}` : ''} onRefresh={refreshExperts} onActivate={async (id, version) => { const activation = await agentApi.activateExpert(id, version); setActiveExpert({ expert_id: activation.expert_id, version: activation.version }); notify('success', '专家已切换'); }} onNotify={(message, tone = 'success') => notify(tone, message)} onMaterializeToWorkspace={async (expert) => { await agentApi.materializeExpertProject(extensionWorkspace.root, expert.id, expert.version); await Promise.all([refreshDevelopment(), refreshExtensionProjects()]); }} workspaceRoot={extensionWorkspace.root} showProjection showAuthoring={false} showMarket={false} /> : installedKind === 'plugin' ? <PluginsPage
      loading={pluginsLoading}
      registry={pluginRegistry}
      catalog={pluginCatalog}
      capabilities={capabilities}
      dashboardEnabled={dashboardEnabled()}
      catalogEnabled={extensionMarketEnabled()}
      desired={extensionDesiredState}
      onLoadVersions={agentApi.pluginVersions}
      onPlanInstall={agentApi.planPluginInstall}
      onImportLocal={() => run(async () => { const registry = await agentApi.importLocalPlugin(); setPluginRegistry(registry); await invalidateBuiltinAiToolContext(); }, '本地插件已导入', '导入本地插件失败')}
      onImportGithub={async (sourceUrl) => { const registry = await agentApi.importGithubPlugin(sourceUrl); setPluginRegistry(registry); await invalidateBuiltinAiToolContext(); notify('success', 'GitHub 插件已导入'); }}
      onInstall={(pluginId, version) => run(async () => { await agentApi.installPlugin(pluginId, version); await refreshPlugins(); await invalidateBuiltinAiToolContext(); }, `已安装插件${version ? ` v${version}` : ''}`, '安装插件失败')}
      onUninstall={(pluginId) => run(async () => { await agentApi.uninstallPlugin(pluginId); await refreshPlugins(); await invalidateBuiltinAiToolContext(); }, '插件已卸载', '卸载插件失败')}
      onRollback={(pluginId) => run(async () => { await agentApi.rollbackPlugin(pluginId); await refreshPlugins(); await invalidateBuiltinAiToolContext(); }, '插件已回滚', '插件回滚失败')}
      onRepair={(pluginId) => run(async () => { await agentApi.repairPlugin(pluginId); await refreshPlugins(); await invalidateBuiltinAiToolContext(); }, '插件已修复，正在重试', '修复插件失败')}
      onSetEnabled={(pluginId, enabled) => run(async () => { await agentApi.setPluginEnabled(pluginId, enabled); await refreshPlugins(); await invalidateBuiltinAiToolContext(); }, enabled ? '插件已启用' : '插件已停用', '更新插件状态失败')}
      onOpenView={(pluginId, viewId) => run(() => agentApi.openPluginView(pluginId, viewId), '插件窗口已打开', '打开插件窗口失败')}
      onCreateShortcut={(pluginId, viewId, title) => run(() => agentApi.createPluginViewShortcut(pluginId, viewId, title), '桌面快捷方式已创建', '创建桌面快捷方式失败')}
      onOpenExtensions={() => setPage('extensions')}
      onBatchUpdateFinished={async () => { await Promise.all([refreshPlugins(), refreshSkills(), refreshExtensionDesiredState()]); await invalidateBuiltinAiToolContext(); }}
    />
      : installedKind === 'skill' ? <SkillsWorkspacePage
      catalog={skillCatalog}
      status={skillStatus}
      workspace={skillWorkspace}
      mcpTargets={mcpTargets}
      error={skillError}
      marketplace={organizationSkills}
	  dashboardEnabled={dashboardEnabled()}
	  catalogEnabled={extensionMarketEnabled()}
	  desired={extensionDesiredState}
	  availablePlugins={availablePlugins}
      busyAction={skillOperation}
      installTargets={installTargets}
      onSyncAll={() => runSkillOperation('sync-all', async () => {
        const result = await agentApi.syncCodexSkills();
        await refreshSkills();
        await invalidateBuiltinAiToolContext();
        const blocked = result.clients
          ? Object.values(result.clients).reduce((count, client) => count + client.blocked.length, 0)
          : result.blocked.length;
        return blocked ? `技能同步完成，${blocked} 项需要处理` : '技能已同步';
      }, '技能同步失败')}
      onClearWorkspace={() => runSkillOperation('workspace-clear', async () => { const next = await agentApi.setSkillWorkspace(); setSkillWorkspace(next); await refreshSkills(); return '已改为全局技能'; }, '恢复全局技能失败')}
      onSyncSkill={(skillId) => runSkillOperation(`sync:${skillId}`, async () => {
        const result = await agentApi.syncCodexSkill(skillId);
        await refreshSkills();
        await invalidateBuiltinAiToolContext();
        return result.rendered.state === 'skipped' ? '技能已同步' : '技能同步完成';
      }, '同步技能失败')}
      onUpdateWorkspace={(skillId) => runSkillOperation(`workspace-update:${skillId}`, async () => {
        const result = await agentApi.updateSkillWorkspace(skillId);
        await refreshSkills();
        await invalidateBuiltinAiToolContext();
        return result.rendered?.state === 'skipped' ? '当前项目已是最新版本' : '当前项目技能已更新';
      }, '更新当前项目技能失败')}
      onInstallToLocation={(skillId) => runSkillOperation(`install-location:${skillId}`, async () => {
        let location = '';
        try {
          location = await agentApi.pickSkillLocation();
        } catch (error) {
          // 取消目录选择是正常操作，不该报成失败。
          if (String(error).includes('已取消')) return '';
          throw error;
        }
        await agentApi.deploySkillToLocation(skillId, location);
        await refreshSkills();
        await invalidateBuiltinAiToolContext();
        return `已安装到 ${location}`;
      }, '安装到位置失败')}
      onUpdateLocation={(skillId, location) => runSkillOperation(`update-location:${skillId}`, async () => {
        await agentApi.deploySkillToLocation(skillId, location);
        await refreshSkills();
        return `已把该技能更新到 ${location}`;
      }, '更新该位置失败')}
      onRemoveLocation={(skillId, location) => runSkillOperation(`remove-location:${skillId}`, async () => {
        const result = await agentApi.removeSkillFromLocation(skillId, location);
        await refreshSkills();
        return result.removed_count ? `已从该目录移除（${result.removed_count} 个工具）` : '该目录里没有这份技能';
      }, '移除该位置失败')}
      // 一次清掉所有"目录已不存在"的台账残渣：同一条命令复用 remove_skill_from_location，
      // 后端在目录缺失时本来就只清记录、不动磁盘，所以批量只是把 N 次点击合成一次。
      onPurgeLocations={(skillId, locations) => runSkillOperation(`purge-locations:${skillId}`, async () => {
        let removed = 0;
        for (const location of locations) {
          const result = await agentApi.removeSkillFromLocation(skillId, location);
          removed += result.removed_count || 0;
        }
        await refreshSkills();
        return removed ? `已清理 ${removed} 条失效记录` : '没有需要清理的记录';
      }, '清理失效记录失败')}
      onSetWorkspaceEnabled={(skillId, enabled) => runSkillOperation(`workspace-enabled:${skillId}`, async () => {
        await agentApi.setSkillWorkspaceEnabled(skillId, enabled);
        await refreshSkills();
        await invalidateBuiltinAiToolContext();
        return enabled ? '当前项目技能已启用' : '当前项目技能已停用';
      }, '更新当前项目技能状态失败')}
      onSyncSkillClient={(skillId, clientId) => runSkillOperation(`register:${clientId}:${skillId}`, async () => {
        const result = await agentApi.syncSkillClient(skillId, clientId);
        await refreshSkills();
        await invalidateBuiltinAiToolContext();
        return result.rendered?.state === 'skipped' ? '技能已是最新版本' : '技能已同步';
      }, '同步技能失败')}
      onPlanMarketplace={agentApi.planOrganizationSkillInstall}
      clientMatrix={clientMatrix}
      onLoadVersions={agentApi.skillVersions}
      onPickSkillLocation={agentApi.pickSkillLocation}
      onInstallMarketplace={(skillId, version, optionalPluginIds, clients, location) => runSkillOperation(`market:${skillId}`, async () => {
        const result = await agentApi.installOrganizationSkill(skillId, version, optionalPluginIds, undefined, undefined, undefined, clients, location || undefined);
        await refreshSkills();
        await invalidateBuiltinAiToolContext();
        return `已安装 ${result.record.manifest.name} v${result.record.manifest.version}`;
      }, '安装技能失败')}
      onRepair={(skillId) => runSkillOperation(`repair:${skillId}`, async () => {
        const result = await agentApi.repairCodexSkill(skillId, true);
        await refreshSkills();
        await invalidateBuiltinAiToolContext();
        return result.backup_root ? '技能已修复，原修改已保留为备份' : '技能已重新安装';
      }, '修复技能失败')}
      onUnregisterClient={(skillId, clientId) => runSkillOperation(`unregister:${clientId}:${skillId}`, async () => {
        const result = await agentApi.unregisterSkillClient(skillId, clientId);
        await refreshSkills();
        await invalidateBuiltinAiToolContext();
        return result.removed.removed ? `已停止同步到 ${result.client_name || clientId}` : `${result.client_name || clientId} 当前未同步`;
      }, '停止同步失败')}
      onUnregisterClients={(skillId) => runSkillOperation(`unregister-all:${skillId}`, async () => {
        const result = await agentApi.unregisterSkillClients(skillId);
        await refreshSkills();
        await invalidateBuiltinAiToolContext();
        const failures = Object.keys(result.failures || {}).length;
        return failures ? `已停止 ${result.removed_count} 个同步，${failures} 个需处理` : `已停止 ${result.removed_count} 个同步`;
      }, '停止同步失败')}
      onUninstall={(skillId) => runSkillOperation(`uninstall:${skillId}`, async () => {
        const result = await agentApi.uninstallCodexSkill(skillId);
        await refreshSkills();
        await invalidateBuiltinAiToolContext();
        if (result.target_kind === 'workspace') return result.removed.removed ? `已从当前项目移除 ${result.removed.skill_id}` : `${result.removed.skill_id} 不在当前项目中`;
        return result.removed.removed ? `已卸载 ${result.removed.skill_id}` : `未卸载 ${result.removed.skill_id}`;
      }, '卸载技能失败')}
      onOpenDirectory={(path) => run(() => agentApi.openFolder(path), '目录已打开', '打开目录失败')}
       onImportLocal={() => run(async () => { const result = await agentApi.importLocalSkill(); await refreshSkills(); await invalidateBuiltinAiToolContext(); return result.record; }, skillWorkspace.valid ? '本地技能已导入到当前项目' : '本地技能已导入', '导入本地技能失败')}
      onImportGithub={async (sourceUrl) => { await agentApi.importGithubSkill(sourceUrl); await refreshSkills(); await invalidateBuiltinAiToolContext(); notify('success', skillWorkspace.valid ? 'GitHub 技能已导入到当前项目' : 'GitHub 技能已导入'); }}
     onOpenExtensions={() => setPage('extensions')}
      />
      : installedKind === 'workflow' ? <WorkflowsPage
        snapshot={workflowCenter}
        runtimeProfiles={acpRuntimeProfiles?.profiles ?? []}
        mode="library"
        onOpenRun={(runId) => { setWorkflowRunTarget(runId); setPage('workflows'); }}
        loading={workflowLoading}
        error={workflowError}
        onRefresh={() => { void refreshWorkflowCenter(); }}
        onLoadRun={loadWorkflowRun}
        onVerify={verifyWorkflowRun}
        onRevealArtifact={(runId, artifactId) => run(() => agentApi.revealWorkflowArtifact(runId, artifactId), undefined, '无法打开输出文件位置')}
        onApprove={approveWorkflowRun}
        onReject={rejectWorkflowRun}
        onResume={resumeWorkflowRun}
        onCancel={cancelWorkflowRun}
        onStart={startWorkflowRun}
        onPreflight={agentApi.preflightWorkflowRun}
        onSaveCredentialFile={async (connectorId, handle) => { try { const result = await agentApi.saveConnectorFileCredential(connectorId, handle); if (!result.cancelled) notify('success', '凭据文件已保存'); return !result.cancelled; } catch (error) { notify('error', formatError(error, '保存凭据文件失败')); throw error; } }}
        onSaveCredentialSecret={async (connectorId, handle, secret) => { try { await agentApi.saveConnectorSecretCredential(connectorId, handle, secret); notify('success', '连接密钥已保存'); } catch (error) { notify('error', formatError(error, '保存连接密钥失败')); throw error; } }}
        onInstallLocal={async () => { try { const picked = await agentApi.pickWorkflowArchive(); if (!picked.path) return; await agentApi.installLocalWorkflowArchive(picked.path); await refreshWorkflowCenter(false, true); notify('success', '本地工作流已安装'); } catch (error) { notify('error', formatError(error, '安装本地工作流失败')); } }}
        onSetEnabled={(packageId, enabled) => run(async () => { await agentApi.setWorkflowEnabled(packageId, enabled); await refreshWorkflowCenter(false, true); }, enabled ? '工作流已启用' : '工作流已停用', '更新工作流状态失败')}
        onRemove={(packageId) => run(async () => { await agentApi.removeWorkflow(packageId); await refreshWorkflowCenter(false, true); }, '工作流已移除', '移除工作流失败')}
        onPickDirectory={async () => { const result = await agentApi.pickWorkspaceDirectory(); return result.path || null; }}
        onOpenExtensions={() => setPage('extensions')}
        onScheduleWorkflow={(workflowId) => { setSchedulePresetTarget(workflowId); setPage('schedules'); }}
        onLoadPresets={(workflowId) => agentApi.workflowPresets(workflowId)}
        onSavePreset={(input) => run(async () => { await agentApi.setWorkflowPreset(input); }, '启动方案已保存', '保存启动方案失败')}
        onDeletePreset={(id) => run(async () => { await agentApi.deleteWorkflowPreset(id); }, '启动方案已删除', '删除启动方案失败')}
      /> : installedKind === 'mcp' ? <div className="mcp-connections-host" ref={mcp.panelRef}><McpConnectionsPanel
        mcp={mcp}
        onBrowseCatalog={openMcpTools}
        // 这些工具是 HiMind AI 在对话里调用的本机能力，和其他已安装能力的口吻保持一致。
        note="这些工具会在对话里提供给 HiMind AI 调用，停用后不再加载。"
      /></div>
      : <ManagedCapabilitiesPanel assetKind="all" desired={extensionDesiredState} loading={extensionDesiredLoading} error={extensionDesiredError} registry={pluginRegistry} skillStatus={skillStatus} workflows={workflowCenter?.workflows || []} onRepairPlugin={(pluginId: string) => run(async () => { await agentApi.repairPlugin(pluginId); await refreshPlugins(); await invalidateBuiltinAiToolContext(); }, '插件已修复，正在重试', '修复插件失败')} />}
    </InstalledPage>;
    if (page === 'development') return <ExtensionDevelopmentPage
      dashboardEnabled={dashboardEnabled()}
      loading={!developmentLoaded}
      workspace={extensionWorkspace}
      workspaces={extensionWorkspaces}
      sources={extensionSources.sources}
      projectsError={extensionProjectsError}
      projects={extensionProjects}
      remoteProjects={extensionRemoteProjects}
      invitations={extensionInvitations}
      accountAuthorized={Boolean(dashboardIdentity?.authorized)}
      pluginDrafts={pluginDrafts}
      skillDrafts={skillDrafts}
      workflowDrafts={workflowDrafts}
      expertDrafts={expertDrafts}
      instructionDrafts={instructionDrafts}
      workflowSubmissions={workflowSubmissions}
      pluginSubmissions={pluginSubmissions}
      skillSubmissions={skillSubmissions}
      availablePlugins={availablePlugins}
      busyAction={developmentOperation}
      onRefresh={() => run(refreshDevelopment, undefined, '刷新扩展项目失败')}
      onCreate={async (input: CreateExtensionProjectInput, parentDir: string) => {
        if (developmentOperation) throw new Error('已有扩展操作正在进行，请稍后重试');
        setDevelopmentOperation('create');
        try {
          const project = await agentApi.createExtensionProject(input, parentDir);
          await refreshDevelopment(true);
          notify('success', `已创建${project.kind === 'plugin' ? '插件' : project.kind === 'workflow' ? '工作流' : '技能'}项目：${project.name}`);
          return project;
        } catch (error) {
          notify('error', formatError(error, '新建扩展项目失败'));
          throw error;
        } finally {
          setDevelopmentOperation(null);
        }
      }}
      onOpenProject={() => runDevelopmentOperation('open', async () => { await agentApi.openExtensionProjects(); await refreshDevelopment(); }, '已添加到扩展开发', '打开扩展项目失败')}
      onAssociateProject={(project: ExtensionRemoteProject) => runDevelopmentOperation(`associate:${project.product_key}`, async () => { await agentApi.associateExtensionProject(project); await refreshDevelopment(); }, '本地项目已关联', '关联本地项目失败')}
      onBuild={(projectId, onProgress) => runDevelopmentOperation(`build:${projectId}`, async () => {
        onProgress?.('building');
        const candidate = await agentApi.buildExtensionProject(projectId);
        onProgress?.('activating');
        if (candidate.kind === 'plugin') {
          await agentApi.testPluginDraft(candidate.draft.manifest.id, candidate.draft.manifest.version);
          await agentApi.confirmPluginDraft(candidate.draft.manifest.id, candidate.draft.manifest.version);
        } else if (candidate.kind === 'skill') {
          await agentApi.testSkillDraft(candidate.draft.manifest.id, candidate.draft.manifest.version);
          await agentApi.confirmSkillDraft(candidate.draft.manifest.id, candidate.draft.manifest.version);
        } else if (candidate.kind === 'workflow') {
          await agentApi.testWorkflowDraft(candidate.draft.package_id, candidate.draft.version);
          await agentApi.confirmWorkflowDraft(candidate.draft.package_id, candidate.draft.version);
        } else if (candidate.kind === 'expert') {
          await agentApi.testExpertDraft(candidate.draft.definition.id, candidate.draft.definition.version);
          await agentApi.confirmExpertDraft(candidate.draft.definition.id, candidate.draft.definition.version);
        } else {
          await agentApi.testInstructionPackDraft(candidate.draft.manifest.id, candidate.draft.manifest.version);
          await agentApi.confirmInstructionPackDraft(candidate.draft.manifest.id, candidate.draft.manifest.version);
        }
        onProgress?.('refreshing');
        await Promise.all([refreshDevelopment(), refreshPlugins(), refreshSkills(), refreshBuiltinAiToolContext()]);
      }, '构建完成，已在本机启用', '构建或启用失败')}
      onDevelopWithAi={(project) => { void openBuiltinAi(project); }}
      onDevelopWorkspaceWithAi={(root: string) => { void openExtensionWorkspaceAi(root); }}
       onSubmit={(kind: ExtensionProjectKind, extensionId: string, version: string) => runDevelopmentOperation(`submit:${kind}:${extensionId}`, async () => { if (kind === 'plugin') await agentApi.submitPluginDraft(extensionId, version); else if (kind === 'skill') await agentApi.submitSkillDraft(extensionId, version); else if (kind === 'workflow') await agentApi.submitWorkflowDraft(extensionId, version); else if (kind === 'expert') await agentApi.submitExpertDraft(extensionId, version); else throw new Error('项目规则请发布到本机规则库'); await refreshDevelopment(); }, '已提交审核', '提交审核失败')}
      onPublishInstruction={(extensionId: string, version: string) => runDevelopmentOperation(`publish-instruction:${extensionId}`, async () => { await agentApi.publishInstructionPackLocally(extensionId, version); await Promise.all([refreshDevelopment(), refreshPlugins()]); }, '已发布到本机规则库', '发布项目规则失败')}
      onOpenFolder={(path) => run(() => agentApi.openFolder(path), '项目目录已打开', '打开项目目录失败')}
      onAddWorkspace={addExtensionWorkspace}
      onRemoveWorkspace={removeExtensionWorkspace}
       onRemove={(projectId) => runDevelopmentOperation(`remove:${projectId}`, async () => { await agentApi.removeExtensionProject(projectId); await refreshDevelopment(); }, '项目已移除', '移除项目失败')}
      onUpdateSource={(projectId: string, input: ExtensionProjectSourceInput, syncRemote: boolean) => runDevelopmentOperation(`source:${projectId}`, async () => { await agentApi.updateExtensionProjectSource(projectId, input, syncRemote); await refreshDevelopment(); }, '代码仓库已保存', '保存代码仓库失败')}
      onSetDistributionTargets={(kind, extensionId, targets) => runDevelopmentOperation(`target:${kind}:${extensionId}`, async () => { await agentApi.setExtensionProjectDistributionTargets(kind, extensionId, targets); await refreshDevelopment(); }, targets ? '分发目标已更新' : '已恢复继承分发单元默认', '保存分发目标失败')}
      onPublishDistribution={(kind, extensionId, version) => runDevelopmentOperation(`publish:${kind}:${extensionId}`, async () => {
        const report = await agentApi.publishExtensionDistribution(kind, extensionId, version);
        await refreshDevelopment();
        const failed = report.outcomes.filter(outcome => outcome.status === 'failed');
        if (failed.length) {
          throw new Error(failed.map(outcome => `${outcome.target}: ${outcome.error || '发布失败'}`).join('；'));
        }
      }, '发布完成', '发布失败')}
      onLoadCollaboration={agentApi.extensionCollaboration}
      onSearchCollaborators={agentApi.extensionCollaboratorOptions}
      onInviteCollaborator={agentApi.inviteExtensionCollaborator}
      onRemoveCollaborator={agentApi.deleteExtensionCollaborator}
      onRespondInvitation={async (invitationId, action) => {
        if (developmentOperation) return;
        setDevelopmentOperation(`invitation:${invitationId}`);
        try {
          await agentApi.respondExtensionCollaborationInvitation(invitationId, action);
          await refreshDevelopment();
          notify('success', action === 'accept' ? '已加入扩展项目' : '已拒绝协作邀请');
        } catch (error) {
          notify('error', formatError(error, '处理协作邀请失败'));
          throw error;
        } finally {
          setDevelopmentOperation(null);
        }
      }}
    />;
    if (page === 'settings' && (!settings || !remoteExecutionSettings || !loginState)) {
      return <SettingsLoadState loading={settingsLoading} error={settingsLoadError} onRetry={refreshSettingsPageData} />;
    }
      if (page === 'settings') return <SettingsPage section={settingsSection} tab={settingsTab} onTabChange={setSettingsTab} logs={logs} onExportDiagnostics={exportDiagnostics} identity={dashboardIdentity} onRevokeAuthorization={revokeDashboardAuthorization} workbenchConnections={workbenchConnections} workbenchConnectionsError={workbenchConnectionsError} workbenchBusyId={workbenchBusyId} workbenchAuthorization={dashboardAuthorization} onRefreshWorkbenchConnections={refreshWorkbenchConnections} onAddWorkbenchConnection={addWorkbenchConnection} onRenameWorkbenchConnection={renameWorkbenchConnection} onRemoveWorkbenchConnection={removeWorkbenchConnection} onSwitchWorkbenchConnection={switchWorkbenchConnection} onEnrollWorkbenchConnection={enrollWorkbenchConnection} onProbeWorkbenchConnection={probeWorkbenchConnection} onAuthorizeWorkbenchConnection={authorizeWorkbenchConnection} independentMode={status?.mode === 'independent' || status?.dashboard_enabled === false} settings={settings} remoteExecutionSettings={remoteExecutionSettings} remoteClients={remoteClients} loginState={loginState} loginModalOpen={loginModalOpen} loginUsername={loginUsername} loginPassword={loginPassword} onOpenLoginModal={openLoginModal} onCloseLoginModal={() => setLoginModalOpen(false)} onUsernameChange={setLoginUsername} onPasswordChange={setLoginPassword} onSaveLogin={() => run(async () => { await agentApi.saveLogin(loginUsername, loginPassword); setLoginPassword(''); setLoginModalOpen(false); await refreshStatus(); await refreshLogin(); await refreshLogs(); }, '内网账号已保存', '保存内网账号失败')} onLogoutLogin={() => run(async () => { await agentApi.logoutLogin(); setLoginPassword(''); setLoginModalOpen(false); await refreshStatus(); await refreshLogin(); await refreshLogs(); }, '已清除内网账号', '清除内网账号失败')} onOpenInnerAdmin={() => run(agentApi.openInnerAdmin)} onRemoteExecutionChange={(next, confirmed) => run(async () => { await agentApi.saveRemoteExecutionSettings(next, confirmed); await refreshRemoteExecutionSettings(); await refreshLogs(); }, next.enabled ? '远程任务设置已更新' : '已关闭远程任务', '远程任务设置更新失败')} onRemoteClientsChange={setRemoteClients} onRuleChange={(requestType, mode) => run(async () => { await agentApi.setRule(requestType, mode); await refreshSettings(); }, '审批规则已更新', '审批规则更新失败')} onApprovalProfileChange={(profile, confirmed, durationSeconds) => run(async () => { await agentApi.setApprovalProfile(profile, confirmed ?? false, durationSeconds); await refreshSettings(); await refreshLogs(); }, '审批档位已更新', '审批档位更新失败')} onApprovalNotificationModeChange={mode => run(async () => { await agentApi.setApprovalNotificationMode(mode); await refreshSettings(); }, '审批提醒方式已更新', '审批提醒方式更新失败')} onTimeoutChange={seconds => run(async () => { await agentApi.setTimeout(seconds); await refreshSettings(); }, '审批超时已更新', '审批超时更新失败')} onAutoStartChange={enabled => run(async () => { const result = await agentApi.setAutoStart(enabled); await refreshSettings(); await refreshLogs(); notify('success', result.auto_start ? '已启用开机自启' : '已关闭开机自启'); }, undefined, '开机自启更新失败')} onUnityEditorSettingsChange={editors => setSettings(current => current ? { ...current, editors } : current)} svnConnections={svnConnections} svnModalOpen={svnModalOpen} svnDraft={svnDraft} onOpenSvnModal={() => setSvnModalOpen(true)} onCloseSvnModal={() => setSvnModalOpen(false)} onSvnDraftChange={setSvnDraft} onSaveSvnConnection={() => run(async () => { await agentApi.saveSvnConnection(svnDraft); setSvnModalOpen(false); await refreshSvnConnections(); }, 'SVN 账号已保存', '保存 SVN 账号失败')} onTestSvnConnection={testSvnConnection} svnTesting={svnTesting} onRemoveSvnConnection={() => run(async () => { await agentApi.removeSvnConnection(); await refreshSvnConnections(); }, 'SVN 账号已删除', '删除 SVN 账号失败')} updateStatus={updateStatus} updateBusy={updateBusy} onCheckUpdate={() => runUpdateOperation(agentApi.checkUpdate, result => result.available_version ? `发现新版本 v${result.available_version}` : '当前已是最新版本')} onDownloadUpdate={() => runUpdateOperation(agentApi.downloadUpdate, result => `v${result.available_version} 更新已下载`)} onCancelUpdateDownload={cancelUpdateDownload} onInstallUpdate={() => runUpdateOperation(agentApi.installUpdate)} onUpdatePreferences={(autoCheck, autoDownload) => runUpdateOperation(() => agentApi.setUpdatePreferences(autoCheck, autoDownload))} />;
    // 到这里 page 只剩设置窗口的面板：设置、AI 连接。运行日志已经是页签，不再有独立页面。
    return null;
  })();

  if (isSettingsWindow) {
    const settingsPanel: SettingsWindowPanel = page === 'ai' ? 'ai' : 'settings';
    const activeRailKey = settingsRailKey(settingsPanel, settingsSection);
    return (
      <SettingsWindow
        activeKey={activeRailKey}
        onSelect={key => {
          if (!isSettingsRailKey(key)) return;
          const next = settingsRailNavigation(key);
          if (next.section) setSettingsSection(next.section);
          setPage(next.panel);
        }}
      >
        <NotificationCenter messages={messages} onClose={dismissNotification} />
        <Suspense fallback={<PageLoadingState />}>{content}</Suspense>
      </SettingsWindow>
    );
  }

  return (
    <Shell
      currentPage={page}
      approvalCount={approvals.length}
      workflowApprovalCount={workflowApprovalCount}
      identity={dashboardIdentity}
      dashboardEnabled={dashboardEnabled()}
      agentVersion={status?.version || '--'}
      updateBusy={updateBusy}
      currentTask={status?.current_task || null}
      activeRunCount={activeRunCount}
      activeRun={activeRunSummary}
      quickPluginViews={quickPluginViews}
      onNavigate={navigate}
      onOpenSettings={() => openSettingsWindow('settings')}
      onOpenPluginView={(pluginId, viewId) => run(() => agentApi.openPluginView(pluginId, viewId), '插件窗口已打开', '打开插件窗口失败')}
      onOpenDashboard={() => run(agentApi.openDashboard)}
      onOpenBuiltinAi={() => { void openBuiltinAi(); }}
      onCheckUpdate={() => runUpdateOperation(agentApi.checkUpdate, result => result.available_version ? `发现新版本 v${result.available_version}` : '当前已是最新版本')}
      onOpenAgentDirectory={() => run(agentApi.openAgentDirectory, '数据目录已打开', '打开数据目录失败')}
      onQuit={() => { void agentApi.quitAgent(); }}
    >
      <NotificationCenter messages={messages} onClose={dismissNotification} />
      <div className={`builtin-ai-page-host ${page === 'builtin-ai' ? 'active' : 'inactive'}`} aria-hidden={page !== 'builtin-ai'}>
        {builtinAiActivated || page === 'builtin-ai' ? builtinAiContent : null}
      </div>
      {page !== 'builtin-ai' ? <Suspense fallback={<PageLoadingState />}>{content}</Suspense> : null}
    </Shell>
  );
}

function SettingsLoadState({ loading, error, onRetry }: { loading: boolean; error: string; onRetry: () => void }) {
  return (
    <>
      <PageHeader title="设置" />
      {loading
        ? <div className="page-loading"><BusyIndicator size={15} />正在读取应用设置</div>
        : <div className="blocker account-blocker" role="alert">
            <ShieldAlert size={18} />
            <div><strong>应用设置读取失败</strong><span>{error || '部分设置暂时不可用，请重新读取。'}</span></div>
            <button type="button" className="btn" onClick={onRetry}><RefreshCw size={15} />重新读取</button>
          </div>}
    </>
  );
}

function PageLoadingState() {
  return <div className="page-loading"><BusyIndicator size={15} />正在打开页面</div>;
}

function withTimeout<T>(promise: Promise<T>, label: string, timeoutMs = 12000): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = window.setTimeout(() => reject(new Error(`${label}读取超时（${timeoutMs / 1000} 秒）`)), timeoutMs);
    promise.then(
      value => {
        window.clearTimeout(timer);
        resolve(value);
      },
      error => {
        window.clearTimeout(timer);
        reject(error);
      },
    );
  });
}

function App() {
  return (
    <ConfirmProvider>
      <AgentApp />
    </ConfirmProvider>
  );
}

createRoot(document.getElementById('root')!).render(<App />);
