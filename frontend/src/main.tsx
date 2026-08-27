import { useEffect, useMemo, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { RefreshCw, ShieldAlert } from 'lucide-react';
import '../styles.css';
import { NotificationCenter, PageHeader } from './components/Common';
import { Shell } from './components/Shell';
import { ApprovalsPage } from './pages/ApprovalsPage';
import { AiConnectionsPage } from './pages/AiConnectionsPage';
import { BuiltinAiPage } from './pages/BuiltinAiPage';
import { DashboardPage } from './pages/DashboardPage';
import { LogsPage } from './pages/LogsPage';
import { PluginsPage } from './pages/PluginsPage';
import { SkillsWorkspacePage } from './pages/SkillsWorkspacePage';
import { ExtensionDevelopmentPage } from './pages/ExtensionDevelopmentPage';
import { SettingsPage } from './pages/SettingsPage';
import { agentApi, type AgentStatus, type AgentUpdateStatus, type ApprovalItem, type ApprovalSettings, type BuiltinAIToolContextSummary, type BuiltinAiWorkspaceTarget, type CapabilityItem, type CodexSkillStatusResponse, type CreateExtensionProjectInput, type DashboardAuthorizationProgress, type DashboardIdentityStatus, type ExtensionCollaborationInvitation, type ExtensionProject, type ExtensionProjectKind, type ExtensionProjectSourceInput, type ExtensionRemoteProject, type ExtensionSourceConfig, type ExtensionSourceSettings, type ExtensionSourceSnapshot, type ExtensionWorkspaceSettings, type McpConnectionTestResult, type McpTargetDescriptor, type SkillCatalogResponse, type OrganizationSkillCatalogItem, type AuthoringPluginDraft, type AuthoringSkillDraft, type PluginSubmissionStatus, type SkillSubmissionStatus, type LogItem, type LoginState, type PluginRegistry, type RemoteClientOverview, type RemoteExecutionSettings, type SkillSyncSettings, type SvnConnection, type SvnConnectionInput } from './services/agentApi';
import { errorDetail, formatError, type PageKey, type UiMessage } from './types';

let nextNotificationId = 1;

function friendlyConnectionError(error: unknown, fallback: string) {
  const detail = errorDetail(error).toLowerCase();
  if (detail.includes('备份并重建')) return '原连接文件格式有误，请选择“备份并重建”。';
  if (detail.includes('permission denied') || detail.includes('access is denied') || detail.includes('拒绝访问')) return '无法修改连接信息，请关闭对应 AI 工具后重试。';
  if (detail.includes('toml') || detail.includes('json') || detail.includes('mcpservers')) return '客户端 MCP 配置文件内容有误，请备份后重建。';
  return fallback;
}

function authorizationFailure(progress: DashboardAuthorizationProgress) {
  if (progress.state === 'denied') return '你暂未同意授权，可以重新发起。';
  if (progress.state === 'expired') return '确认已超时，请重新发起。';
  return '未能完成工作台账号授权，请检查网络后重试。';
}

function App() {
  const [page, setPage] = useState<PageKey>('dashboard');
  const [status, setStatus] = useState<AgentStatus | null>(null);
  const statusRef = useRef<AgentStatus | null>(null);
  const [updateStatus, setUpdateStatus] = useState<AgentUpdateStatus | null>(null);
  const [updateBusy, setUpdateBusy] = useState(false);
  const [dashboardIdentity, setDashboardIdentity] = useState<DashboardIdentityStatus | null>(null);
  const [dashboardAuthorization, setDashboardAuthorization] = useState<DashboardAuthorizationProgress | null>(null);
  const [builtinAiToolContext, setBuiltinAiToolContext] = useState<BuiltinAIToolContextSummary | null>(null);
  const [builtinAiActivated, setBuiltinAiActivated] = useState(false);
  const [builtinAiWorkspaceRequest, setBuiltinAiWorkspaceRequest] = useState<{ target: BuiltinAiWorkspaceTarget; revision: number }>({ target: null, revision: 0 });
  const [mcpTestResult, setMcpTestResult] = useState<McpConnectionTestResult | null>(null);
  const [mcpTargets, setMcpTargets] = useState<McpTargetDescriptor[]>([]);
  const [aiOperation, setAiOperation] = useState<string | null>(null);
  const [approvals, setApprovals] = useState<ApprovalItem[]>([]);
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
  const [capabilities, setCapabilities] = useState<CapabilityItem[]>([]);
  const [pluginCatalog, setPluginCatalog] = useState<import('./services/agentApi').PluginCatalogItem[]>([]);
  const [pluginDrafts, setPluginDrafts] = useState<AuthoringPluginDraft[]>([]);
  const [pluginSubmissions, setPluginSubmissions] = useState<PluginSubmissionStatus[]>([]);
  const [skillCatalog, setSkillCatalog] = useState<SkillCatalogResponse | null>(null);
  const [skillStatus, setSkillStatus] = useState<CodexSkillStatusResponse | null>(null);
  const [organizationSkills, setOrganizationSkills] = useState<OrganizationSkillCatalogItem[]>([]);
  const [skillMarketError, setSkillMarketError] = useState<string | null>(null);
  const [skillDrafts, setSkillDrafts] = useState<AuthoringSkillDraft[]>([]);
  const [skillSubmissions, setSkillSubmissions] = useState<SkillSubmissionStatus[]>([]);
  const [skillError, setSkillError] = useState<string | null>(null);
  const [skillOperation, setSkillOperation] = useState<string | null>(null);
  const [extensionProjects, setExtensionProjects] = useState<ExtensionProject[]>([]);
  const [extensionWorkspace, setExtensionWorkspace] = useState<ExtensionWorkspaceSettings>({ configured: false, valid: false, root: '', catalog_path: '', repository: '', default_branch: '', extension_count: 0, error: '' });
  const [extensionSources, setExtensionSources] = useState<ExtensionSourceSettings>({ schema_version: 1, sources: [] });
  const [extensionSourceSnapshot, setExtensionSourceSnapshot] = useState<ExtensionSourceSnapshot | null>(null);
  const [extensionSourcesLoading, setExtensionSourcesLoading] = useState(false);
  const [extensionSourcesError, setExtensionSourcesError] = useState('');
  const [extensionRemoteProjects, setExtensionRemoteProjects] = useState<ExtensionRemoteProject[]>([]);
  const [extensionInvitations, setExtensionInvitations] = useState<ExtensionCollaborationInvitation[]>([]);
  const [developmentOperation, setDevelopmentOperation] = useState<string | null>(null);
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

  async function refreshStatus() {
    const next = await agentApi.status();
    statusRef.current = next;
    setStatus(next);
    return next;
  }
  function dashboardEnabled() {
    const current = statusRef.current || status;
    if (!current) return false;
    return current.mode !== 'independent' && current.dashboard_enabled !== false;
  }
  function extensionMarketEnabled() {
    return dashboardEnabled() || extensionSources.sources.some(source => source.enabled);
  }
  async function refreshUpdateStatus() { setUpdateStatus(await agentApi.updateStatus()); }
  async function refreshDashboardIdentity() {
    const identity = await agentApi.dashboardIdentity();
    setDashboardIdentity(identity);
    if (identity.state !== 'independent') {
      // identity_status may finish the delayed local SVN bootstrap. Read the file only after it returns.
      try { await refreshSvnConnections(); } catch (error) { console.error(error); }
    }
  }
  async function refreshMcpTargets() { setMcpTargets(await agentApi.mcpTargets()); }
  async function refreshBuiltinAiToolContext() {
    try {
      setBuiltinAiToolContext(await agentApi.builtinAiToolContextSummary());
    } catch {
      setBuiltinAiToolContext(null);
    }
  }
  async function refreshApprovals() { setApprovals(await agentApi.approvals()); }
  async function refreshSettings() { setSettings(await agentApi.settings()); }
  async function refreshRemoteExecutionSettings() { setRemoteExecutionSettings(await agentApi.remoteExecutionSettings()); }
  async function refreshLogin() { setLoginState(await agentApi.login()); }
  async function refreshSettingsPageData() {
    setSettingsLoading(true);
    setSettingsLoadError('');
    try {
      const [settingsResult, remoteExecutionResult, loginResult, remoteClientsResult] = await Promise.allSettled([
        withTimeout(agentApi.settings(), '审批设置'),
        withTimeout(agentApi.remoteExecutionSettings(), '远程任务设置'),
        withTimeout(agentApi.login(), '本地登录状态'),
        withTimeout(agentApi.remoteClients(), '远程运维客户端配置'),
      ] as const);
      const errors: string[] = [];
      if (settingsResult.status === 'fulfilled') setSettings(settingsResult.value);
      else {
        setSettings(null);
        errors.push(formatError(settingsResult.reason, '审批设置读取失败'));
      }
      if (remoteExecutionResult.status === 'fulfilled') setRemoteExecutionSettings(remoteExecutionResult.value);
      else {
        setRemoteExecutionSettings(null);
        errors.push(formatError(remoteExecutionResult.reason, '远程任务设置读取失败'));
      }
      if (loginResult.status === 'fulfilled') setLoginState(loginResult.value);
      else {
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
      setSettingsLoadError(formatError(error, 'Agent 配置读取失败'));
    } finally {
      setSettingsLoading(false);
    }
  }
  async function refreshLogs() { setLogs(await agentApi.logs()); }
  async function refreshExtensionProjects() {
    setExtensionProjects(await agentApi.extensionProjects());
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
    setPluginsLoading(true);
    try {
      const [registry, capabilityItems] = await Promise.all([
        withTimeout(agentApi.plugins(), '本机插件注册表'),
        withTimeout(agentApi.capabilities(), '本机能力清单'),
      ]);
      setPluginRegistry(registry);
      setCapabilities(Array.isArray(capabilityItems) ? capabilityItems : []);
      try {
        const catalog = await withTimeout(agentApi.pluginCatalog(), '插件市场');
        setPluginCatalog(Array.isArray(catalog) ? catalog : []);
      } catch (error) {
        setPluginCatalog([]);
        console.error('Plugin catalog unavailable', error);
      }
    } catch (error) {
      setPluginRegistry(null);
      setCapabilities([]);
      setPluginCatalog([]);
      console.error('Plugin registry unavailable', error);
    } finally {
      setPluginsLoading(false);
    }
  }

  async function refreshExtensionDesiredState() {
    if (!dashboardEnabled()) {
      setExtensionDesiredState(null);
      setExtensionDesiredError(null);
      setExtensionDesiredLoading(false);
      return;
    }
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
  }

  async function refreshSkills() {
    const [catalogResult, statusResult, marketResult] = await Promise.allSettled([
      withTimeout(agentApi.skillCatalog(), '本地 Skill 目录'),
      withTimeout(agentApi.codexSkillStatus(), 'AI 工具技能状态'),
      withTimeout(agentApi.organizationSkillCatalog(), '技能市场'),
    ]);
    const errors: string[] = [];
    if (catalogResult.status === 'fulfilled') {
      setSkillCatalog(catalogResult.value);
    } else {
      setSkillCatalog(null);
      errors.push(formatError(catalogResult.reason, '本机技能读取失败'));
    }
    if (statusResult.status === 'fulfilled') {
      setSkillStatus(statusResult.value);
    } else {
      setSkillStatus(null);
      errors.push(formatError(statusResult.reason, 'AI 工具技能状态读取失败'));
    }
	if (marketResult.status === 'fulfilled') {
	  setOrganizationSkills(Array.isArray(marketResult.value) ? marketResult.value : []);
	  setSkillMarketError(null);
	} else {
	  setOrganizationSkills([]);
	  setSkillMarketError(formatError(marketResult.reason, '技能市场暂不可用'));
	}
    setSkillError(errors.length ? errors.join('；') : null);
  }

  async function refreshDevelopment() {
    const [projects, pluginDraftResult, skillDraftResult] = await Promise.allSettled([
      agentApi.extensionProjects(),
      agentApi.pluginDrafts(),
      agentApi.skillDrafts(),
    ]);
    try { setExtensionWorkspace(await agentApi.extensionWorkspace()); }
    catch (error) { console.error('Extension workspace unavailable', error); }
    if (projects.status === 'fulfilled') setExtensionProjects(projects.value || []);
    if (pluginDraftResult.status === 'fulfilled') setPluginDrafts(pluginDraftResult.value || []);
    if (skillDraftResult.status === 'fulfilled') setSkillDrafts(skillDraftResult.value || []);
    if (!dashboardEnabled()) {
      setExtensionRemoteProjects([]);
      setPluginSubmissions([]);
      setSkillSubmissions([]);
      setExtensionInvitations([]);
      return;
    }
    const [remoteProjects, pluginSubmissionResult, skillSubmissionResult, invitationResult] = await Promise.allSettled([
      agentApi.extensionCollaborationProjects(),
      agentApi.pluginSubmissions(),
      agentApi.skillSubmissions(),
      agentApi.extensionCollaborationInvitations(),
    ]);
    if (remoteProjects.status === 'fulfilled') setExtensionRemoteProjects(remoteProjects.value || []);
    if (pluginSubmissionResult.status === 'fulfilled') setPluginSubmissions(pluginSubmissionResult.value || []);
    if (skillSubmissionResult.status === 'fulfilled') setSkillSubmissions(skillSubmissionResult.value || []);
    if (invitationResult.status === 'fulfilled') setExtensionInvitations(invitationResult.value || []);
  }
  async function refreshExtensionSourceSettings() {
    setExtensionSources(await agentApi.extensionSources());
  }
  async function refreshExtensionSources() {
    setExtensionSourcesLoading(true);
    setExtensionSourcesError('');
    try {
      const settings = await agentApi.extensionSources();
      setExtensionSources(settings);
      try {
        setExtensionSourceSnapshot(await agentApi.extensionSourceSnapshot());
      } catch (error) {
        setExtensionSourceSnapshot(null);
        setExtensionSourcesError(formatError(error, '扩展源刷新失败'));
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
      await Promise.all([refreshPlugins(), refreshSkills()]);
    } finally {
      setExtensionSourcesLoading(false);
    }
  }
  async function updateExtensionSource(source: ExtensionSourceConfig, enabled: boolean, autoUpdate: boolean, verification: ExtensionSourceConfig['verification']) {
    setExtensionSourcesLoading(true);
    try {
      setExtensionSources(await agentApi.updateExtensionSource(source.id, enabled, autoUpdate, verification));
      await refreshExtensionSources();
      await Promise.all([refreshPlugins(), refreshSkills()]);
    } finally {
      setExtensionSourcesLoading(false);
    }
  }
  async function removeExtensionSource(sourceId: string) {
    setExtensionSourcesLoading(true);
    try {
      setExtensionSources(await agentApi.removeExtensionSource(sourceId));
      await refreshExtensionSources();
      await Promise.all([refreshPlugins(), refreshSkills()]);
    } finally {
      setExtensionSourcesLoading(false);
    }
  }
  async function selectExtensionWorkspace() {
    const workspace = await agentApi.selectExtensionWorkspace();
    setExtensionWorkspace(workspace);
    await refreshDevelopment();
    notify('success', `已选择扩展聚合仓库，共 ${workspace.extension_count} 个扩展`);
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

  async function refreshAll() {
    let initialStatus: AgentStatus | null = null;
    let statusRead = true;
    try {
      initialStatus = await refreshStatus();
    } catch (error) {
      statusRead = false;
      console.error('Agent status unavailable during initialization', error);
    }
    const connected = statusRead && initialStatus
      ? initialStatus.mode !== 'independent' && initialStatus.dashboard_enabled !== false
      : false;
    const results = await Promise.allSettled([
      refreshUpdateStatus(),
      refreshDashboardIdentity(),
      refreshMcpTargets(),
      refreshBuiltinAiToolContext(),
      refreshApprovals(),
      refreshSettingsPageData(),
      refreshPlugins(),
      refreshSkills(),
      refreshLogs(),
      refreshDevelopment(),
      refreshExtensionSourceSettings(),
      ...(connected ? [refreshExtensionDesiredState()] : []),
    ]);
    for (const result of results) {
      if (result.status === 'rejected') throw result.reason;
    }
  }

  useEffect(() => {
    refreshAll().catch(error => notify('error', formatError(error, 'Agent 面板初始化失败')));
    const timer = window.setInterval(() => {
      Promise.all([refreshStatus(), refreshUpdateStatus(), refreshApprovals(), refreshLogin()]).catch(console.error);
    }, 5000);
    const identityTimer = window.setInterval(() => refreshDashboardIdentity().catch(console.error), 30000);
    const toolContextTimer = window.setInterval(() => refreshBuiltinAiToolContext().catch(console.error), 30000);
    const reviewTimer = window.setInterval(() => refreshReviewProgress().catch(console.error), 30000);
    return () => {
      window.clearInterval(timer);
      window.clearInterval(identityTimer);
      window.clearInterval(toolContextTimer);
      window.clearInterval(reviewTimer);
    };
  }, []);

  useEffect(() => {
    if (page !== 'settings') return;
    const refreshSettingsOnVisibility = () => {
      if (document.visibilityState === 'hidden') return;
      Promise.all([refreshSettingsPageData(), refreshDashboardIdentity()]).catch(console.error);
    };
    refreshSettingsOnVisibility();
    const timer = window.setInterval(refreshSettingsOnVisibility, 15000);
    window.addEventListener('focus', refreshSettingsOnVisibility);
    document.addEventListener('visibilitychange', refreshSettingsOnVisibility);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener('focus', refreshSettingsOnVisibility);
      document.removeEventListener('visibilitychange', refreshSettingsOnVisibility);
    };
  }, [page]);

  useEffect(() => {
    if (page !== 'plugins' && page !== 'skills' && page !== 'development') return;
    const refreshCurrentPage = () => {
      if (document.visibilityState === 'hidden') return;
      const operation = page === 'plugins'
        ? Promise.all([refreshExtensionDesiredState(), refreshPlugins()])
        : page === 'skills'
          ? Promise.all([refreshExtensionDesiredState(), refreshSkills()])
          : refreshDevelopment();
      operation.catch(console.error);
    };
    const timer = window.setInterval(refreshCurrentPage, 15000);
    window.addEventListener('focus', refreshCurrentPage);
    document.addEventListener('visibilitychange', refreshCurrentPage);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener('focus', refreshCurrentPage);
      document.removeEventListener('visibilitychange', refreshCurrentPage);
    };
  }, [page]);

  useEffect(() => {
    if (page !== 'logs') return;
    const refreshVisibleLogs = () => {
      if (document.visibilityState === 'hidden') return;
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
  }, [page]);

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
          await refreshDashboardIdentity();
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

  async function openBuiltinAi(project?: ExtensionProject) {
    if (project) {
      try {
        await agentApi.prepareExtensionAuthoring();
        await Promise.all([refreshPlugins(), refreshSkills()]);
      } catch (error) {
        notify('error', formatError(error, '扩展创作助手尚未就绪，请检查扩展源'));
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

  async function invalidateBuiltinAiToolContext() {
    try {
      await agentApi.reloadBuiltinAiToolContext();
    } catch (error) {
      console.error('HiMind AI 工具上下文刷新失败', error);
    }
    setBuiltinAiWorkspaceRequest(current => ({ ...current, revision: current.revision + 1 }));
  }

  async function openExtensionWorkspaceAi() {
    if (!extensionWorkspace.valid) {
      notify('error', extensionWorkspace.error || '请先选择有效的扩展聚合仓库');
      return;
    }
    try {
      await agentApi.prepareExtensionAuthoring();
      await Promise.all([refreshPlugins(), refreshSkills()]);
    } catch (error) {
      notify('error', formatError(error, '扩展创作助手尚未就绪，请检查扩展源'));
      return;
    }
    setBuiltinAiWorkspaceRequest(current => {
      if (current.target?.kind === 'extension-workspace' && current.target.path === extensionWorkspace.root) return current;
      return {
        target: { kind: 'extension-workspace', name: '扩展聚合仓库', path: extensionWorkspace.root },
        revision: current.revision + 1,
      };
    });
    setBuiltinAiActivated(true);
    setPage('builtin-ai');
  }

  async function runSkillOperation(key: string, action: () => Promise<string>, fallback: string) {
    if (skillOperation) return;
    setSkillOperation(key);
    try {
      const message = await action();
      notify('success', message);
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
        description: item.description || (item.development ? '本机开发候选插件' : '本机插件'),
        author_name: item.author_name,
        categories: [],
        governance: item.governance || 'optional',
        version: item.version || '0.0.0',
        release_notes: '',
        min_agent_version: item.min_agent_version || '',
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
    setAiOperation('identity');
    try {
      await agentApi.revokeDashboardAuthorization();
      setDashboardAuthorization(null);
      await refreshDashboardIdentity();
      notify('success', '已取消工作台账号授权');
    } catch (error) {
      notify('error', friendlyConnectionError(error, '暂时无法取消工作台账号授权，请稍后重试。'));
    } finally {
      setAiOperation(null);
    }
  }

  async function applyMcpTarget(targetId: string, resetInvalid = false) {
    if (aiOperation) return;
    setAiOperation(`target:${targetId}`);
    try {
      const result = await agentApi.applyMcpRegistration(targetId, resetInvalid);
      await refreshMcpTargets();
      notify('success', result.changed ? `${result.target.name} 的 MCP 注册已更新` : `${result.target.name} 的 MCP 注册已就绪`);
    } catch (error) {
      notify('error', friendlyConnectionError(error, 'MCP 注册失败，请关闭对应 AI 工具后重试。'));
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
      notify('success', result.changed ? `${result.target.name} 的 MCP 注册已移除` : `${result.target.name} 当前没有 HiMind MCP 注册`);
    } catch (error) {
      notify('error', friendlyConnectionError(error, '取消注册失败，请关闭对应 AI 工具后重试。'));
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
        notify('error', `已注册 ${result.results.length} 个 AI 工具，${result.failures.length} 个需要单独处理`);
      } else {
        notify('success', result.results.length ? `已完成 ${result.results.length} 个 AI 工具的 MCP 注册` : '已发现的 AI 工具均已注册');
      }
    } catch (error) {
      notify('error', friendlyConnectionError(error, '批量注册 MCP 服务失败。'));
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
        notify('error', `已取消 ${result.results.length} 个注册，${result.failures.length} 个需处理`);
      } else {
        notify('success', result.results.length ? `已取消 ${result.results.length} 个注册` : '没有可取消的注册');
      }
    } catch (error) {
      notify('error', friendlyConnectionError(error, '取消注册失败。'));
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
      notify('success', 'MCP 服务正常');
    } catch (error) {
      notify('error', friendlyConnectionError(error, '本机服务检查失败，请重新启动 HiMind Agent。'));
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
      onOpenSettings={() => setPage('settings')}
      onOpenAiConnections={() => setPage('ai')}
      onOpenPlugins={() => setPage('plugins')}
      onOpenSkills={() => setPage('skills')}
      onToolContextChanged={() => { void refreshBuiltinAiToolContext(); void invalidateBuiltinAiToolContext(); }}
      toolSummary={builtinAiToolContext || {
        skills: skillCatalog?.items?.length || 0,
        mcp_services: 1,
      }}
      workspaceTarget={builtinAiWorkspaceRequest.target}
      workspaceRequestRevision={builtinAiWorkspaceRequest.revision}
    />;

  const content = (() => {
    if (page === 'builtin-ai') return null;
    if (page === 'dashboard') return <DashboardPage
      status={status}
      approvals={approvals}
      remoteExecutionSettings={remoteExecutionSettings}
      mcpTargets={mcpTargets}
      identity={dashboardIdentity}
      authorization={dashboardAuthorization}
      identityBusy={aiOperation === 'identity'}
      updateStatus={updateStatus}
      updateBusy={updateBusy}
      onOpenDashboard={() => run(agentApi.openDashboard)}
      onStartAuthorization={startDashboardAuthorization}
      onCancelAuthorization={cancelDashboardAuthorization}
      onOpenAuthorization={() => run(agentApi.openDashboardAuthorizationPage)}
      onRefreshIdentity={() => run(refreshDashboardIdentity)}
      onRevokeAuthorization={revokeDashboardAuthorization}
      onCheckUpdate={() => runUpdateOperation(agentApi.checkUpdate, result => result.available_version ? `发现新版本 v${result.available_version}` : '当前已是最新版本')}
      onDownloadUpdate={() => runUpdateOperation(agentApi.downloadUpdate, result => `v${result.available_version} 更新已下载`)}
      onInstallUpdate={() => runUpdateOperation(agentApi.installUpdate)}
    />;
    if (page === 'ai') return <AiConnectionsPage
      identity={dashboardIdentity}
      dashboardEnabled={dashboardEnabled()}
      testResult={mcpTestResult}
      busyAction={aiOperation}
      onOpenAccount={() => setPage('dashboard')}
      targets={mcpTargets}
      onRefresh={() => run(async () => { await Promise.all([refreshDashboardIdentity(), refreshMcpTargets()]); })}
      onApplyTarget={applyMcpTarget}
      onApplyAll={applyAllMcpTargets}
      onRemoveAll={removeAllMcpTargets}
      onRemoveTarget={removeMcpTarget}
      onOpenDirectory={(path) => run(() => agentApi.openFolder(path))}
      onTest={testMcpConnection}
    />;
    if (page === 'approvals') return <ApprovalsPage approvals={approvals} onRefresh={() => run(refreshApprovals)} onRespond={(id, approved) => run(async () => { await agentApi.respondApproval(id, approved); await refreshApprovals(); await refreshStatus(); }, undefined, '审批处理失败')} />;
    if (page === 'plugins') return <PluginsPage loading={pluginsLoading} registry={pluginRegistry} catalog={pluginCatalog} capabilities={capabilities} dashboardEnabled={dashboardEnabled()} marketEnabled={extensionMarketEnabled()} desired={extensionDesiredState} desiredLoading={extensionDesiredLoading} desiredError={extensionDesiredError} skillStatus={skillStatus} onQueryCatalog={agentApi.queryPluginCatalog} onRefresh={() => run(async () => { await Promise.all([refreshExtensionDesiredState(), refreshPlugins()]); })} onLoadVersions={agentApi.pluginVersions} onPlanInstall={agentApi.planPluginInstall} onImportLocal={() => run(async () => { const registry = await agentApi.importLocalPlugin(); setPluginRegistry(registry); await invalidateBuiltinAiToolContext(); }, '本地插件已导入', '导入本地插件失败')} onImportGithub={async (sourceUrl) => { const registry = await agentApi.importGithubPlugin(sourceUrl); setPluginRegistry(registry); await invalidateBuiltinAiToolContext(); notify('success', 'GitHub 插件已导入'); }} onInstall={(pluginId, version) => run(async () => { await agentApi.installPlugin(pluginId, version); await refreshPlugins(); await invalidateBuiltinAiToolContext(); }, `已安装插件${version ? ` v${version}` : ''}`, '安装插件失败')} onUninstall={(pluginId) => run(async () => { await agentApi.uninstallPlugin(pluginId); await refreshPlugins(); await invalidateBuiltinAiToolContext(); }, '插件已卸载', '卸载插件失败')} onRollback={(pluginId) => run(async () => { await agentApi.rollbackPlugin(pluginId); await refreshPlugins(); await invalidateBuiltinAiToolContext(); }, '插件已回滚', '插件回滚失败')} onSetEnabled={(pluginId, enabled) => run(async () => { await agentApi.setPluginEnabled(pluginId, enabled); await refreshPlugins(); await invalidateBuiltinAiToolContext(); }, enabled ? '插件已启用' : '插件已停用', '更新插件状态失败')} onOpenView={(pluginId, viewId) => run(() => agentApi.openPluginView(pluginId, viewId), '插件窗口已打开', '打开插件窗口失败')} onCreateShortcut={(pluginId, viewId, title) => run(() => agentApi.createPluginViewShortcut(pluginId, viewId, title), '桌面快捷方式已创建', '创建桌面快捷方式失败')} />;
    if (page === 'skills') return <SkillsWorkspacePage
      catalog={skillCatalog}
      status={skillStatus}
      mcpTargets={mcpTargets}
      error={skillError}
      marketplace={organizationSkills}
      marketplaceError={skillMarketError}
	  dashboardEnabled={dashboardEnabled()}
	  marketEnabled={extensionMarketEnabled()}
	  desired={extensionDesiredState}
	  desiredLoading={extensionDesiredLoading}
	  desiredError={extensionDesiredError}
	  pluginRegistry={pluginRegistry}
	  onQueryMarketplace={agentApi.queryOrganizationSkillCatalog}
	  availablePlugins={availablePlugins}
      busyAction={skillOperation}
      onRefresh={() => run(async () => { await Promise.all([refreshExtensionDesiredState(), refreshSkills()]); })}
      onSyncAll={() => runSkillOperation('sync-all', async () => {
        const result = await agentApi.syncCodexSkills();
        await refreshSkills();
        await invalidateBuiltinAiToolContext();
        const blocked = result.clients
          ? Object.values(result.clients).reduce((count, client) => count + client.blocked.length, 0)
          : result.blocked.length;
        return blocked ? `技能同步完成，${blocked} 项需要处理` : '技能已同步';
      }, '技能同步失败')}
      onSyncSkill={(skillId) => runSkillOperation(`sync:${skillId}`, async () => {
        const result = await agentApi.syncCodexSkill(skillId);
        await refreshSkills();
        await invalidateBuiltinAiToolContext();
        return result.rendered.state === 'skipped' ? '技能已注册' : '技能注册完成';
      }, '注册技能失败')}
      onSyncSkillClient={(skillId, clientId) => runSkillOperation(`register:${clientId}:${skillId}`, async () => {
        const result = await agentApi.syncSkillClient(skillId, clientId);
        await refreshSkills();
        await invalidateBuiltinAiToolContext();
        return result.rendered?.state === 'skipped' ? '技能已是最新版本' : '技能已注册';
      }, '注册技能失败')}
      syncMode={skillStatus?.sync_mode || 'copy'}
      onSetSyncMode={(mode: SkillSyncSettings['mode']) => runSkillOperation('sync-mode', async () => {
        await agentApi.setSkillSyncMode(mode);
        await agentApi.syncCodexSkills();
        await refreshSkills();
        await invalidateBuiltinAiToolContext();
        return '安装方式已更新';
      }, '更新安装方式失败')}
      onPlanMarketplace={agentApi.planOrganizationSkillInstall}
      onLoadVersions={agentApi.skillVersions}
      onInstallMarketplace={(skillId, version, optionalPluginIds) => runSkillOperation(`market:${skillId}`, async () => {
        const result = await agentApi.installOrganizationSkill(skillId, version, optionalPluginIds);
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
        return result.removed.removed ? `${result.client_name || clientId} 已取消注册` : `${result.client_name || clientId} 当前未注册`;
      }, '取消注册失败')}
      onUnregisterClients={(skillId) => runSkillOperation(`unregister-all:${skillId}`, async () => {
        const result = await agentApi.unregisterSkillClients(skillId);
        await refreshSkills();
        await invalidateBuiltinAiToolContext();
        const failures = Object.keys(result.failures || {}).length;
        return failures ? `已取消 ${result.removed_count} 个注册，${failures} 个需处理` : `已取消 ${result.removed_count} 个注册`;
      }, '取消注册失败')}
      onUninstall={(skillId) => runSkillOperation(`uninstall:${skillId}`, async () => {
        const result = await agentApi.uninstallCodexSkill(skillId);
        await refreshSkills();
        await invalidateBuiltinAiToolContext();
        return result.removed.removed ? `已卸载 ${result.removed.skill_id}` : `未卸载 ${result.removed.skill_id}`;
      }, '卸载技能失败')}
      onOpenDirectory={(path) => run(() => agentApi.openFolder(path), '目录已打开', '打开目录失败')}
      onImportLocal={() => run(async () => { const record = await agentApi.importLocalSkill(); await refreshSkills(); await invalidateBuiltinAiToolContext(); return record; }, '本地 Skill 已导入', '导入本地 Skill 失败')}
      onImportGithub={async (sourceUrl) => { await agentApi.importGithubSkill(sourceUrl); await refreshSkills(); await invalidateBuiltinAiToolContext(); notify('success', 'GitHub Skill 已导入'); }}
      onOpenAiConnections={() => setPage('ai')}
    />;
    if (page === 'development') return <ExtensionDevelopmentPage
      dashboardEnabled={dashboardEnabled()}
      workspace={extensionWorkspace}
      extensionSources={extensionSources}
      extensionSourceSnapshot={extensionSourceSnapshot}
      extensionSourcesLoading={extensionSourcesLoading}
      extensionSourcesError={extensionSourcesError}
      projects={extensionProjects}
      remoteProjects={extensionRemoteProjects}
      invitations={extensionInvitations}
      accountAuthorized={Boolean(dashboardIdentity?.authorized)}
      pluginDrafts={pluginDrafts}
      skillDrafts={skillDrafts}
      pluginSubmissions={pluginSubmissions}
      skillSubmissions={skillSubmissions}
      availablePlugins={availablePlugins}
      busyAction={developmentOperation}
      onRefresh={() => run(refreshDevelopment, undefined, '刷新扩展项目失败')}
      onRefreshSources={refreshExtensionSources}
      onAddSource={addExtensionSource}
      onUpdateSourceConfig={updateExtensionSource}
      onRemoveSource={removeExtensionSource}
      onSelectWorkspace={() => run(selectExtensionWorkspace, undefined, '选择扩展仓库失败')}
      onCreate={async (input: CreateExtensionProjectInput) => {
        if (developmentOperation) return;
        setDevelopmentOperation('create');
        try {
          const project = await agentApi.createExtensionProject(input);
          await refreshDevelopment();
          notify('success', `已创建${project.kind === 'plugin' ? '插件' : '技能'}项目：${project.name}`);
        } catch (error) {
          notify('error', formatError(error, '新建扩展项目失败'));
          throw error;
        } finally {
          setDevelopmentOperation(null);
        }
      }}
      onOpenProject={() => runDevelopmentOperation('open', async () => { await agentApi.openExtensionProjects(); await refreshDevelopment(); }, '项目或聚合仓库已加入工作台', '打开扩展项目失败')}
      onAssociateProject={(project: ExtensionRemoteProject) => runDevelopmentOperation(`associate:${project.product_key}`, async () => { await agentApi.associateExtensionProject(project); await refreshDevelopment(); }, '本地项目已关联', '关联本地项目失败')}
      onBuild={(projectId, onProgress) => runDevelopmentOperation(`build:${projectId}`, async () => {
        onProgress?.('building');
        const candidate = await agentApi.buildExtensionProject(projectId);
        onProgress?.('activating');
        if (candidate.kind === 'plugin') {
          await agentApi.testPluginDraft(candidate.draft.manifest.id, candidate.draft.manifest.version);
        } else {
          await agentApi.testSkillDraft(candidate.draft.manifest.id, candidate.draft.manifest.version);
        }
        onProgress?.('refreshing');
        await Promise.all([refreshDevelopment(), refreshPlugins(), refreshSkills(), refreshBuiltinAiToolContext()]);
      }, '构建完成，已启用到本机 AI 工具', '构建或启用失败')}
      onDevelopWithAi={(project) => { void openBuiltinAi(project); }}
      onDevelopWorkspace={() => { void openExtensionWorkspaceAi(); }}
      onSubmit={(kind: ExtensionProjectKind, extensionId: string, version: string) => runDevelopmentOperation(`submit:${kind}:${extensionId}`, async () => { if (kind === 'plugin') await agentApi.submitPluginDraft(extensionId, version); else await agentApi.submitSkillDraft(extensionId, version); await refreshDevelopment(); }, '已提交 HiMind 工作台审核', '提交审核失败')}
      onOpenFolder={(path) => run(() => agentApi.openFolder(path), '项目目录已打开', '打开项目目录失败')}
      onRemove={(projectId) => runDevelopmentOperation(`remove:${projectId}`, async () => { await agentApi.removeExtensionProject(projectId); await refreshDevelopment(); }, '项目已移出工作台', '移出项目失败')}
      onUpdateSource={(projectId: string, input: ExtensionProjectSourceInput, syncRemote: boolean) => runDevelopmentOperation(`source:${projectId}`, async () => { await agentApi.updateExtensionProjectSource(projectId, input, syncRemote); await refreshDevelopment(); }, '代码仓库已保存', '保存代码仓库失败')}
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
     if (page === 'settings') return <SettingsPage settings={settings} remoteExecutionSettings={remoteExecutionSettings} remoteClients={remoteClients} loginState={loginState} loginModalOpen={loginModalOpen} loginUsername={loginUsername} loginPassword={loginPassword} onOpenLoginModal={openLoginModal} onCloseLoginModal={() => setLoginModalOpen(false)} onUsernameChange={setLoginUsername} onPasswordChange={setLoginPassword} onSaveLogin={() => run(async () => { await agentApi.saveLogin(loginUsername, loginPassword); setLoginPassword(''); setLoginModalOpen(false); await refreshStatus(); await refreshLogin(); await refreshLogs(); }, '内网账号已保存', '保存内网账号失败')} onLogoutLogin={() => run(async () => { await agentApi.logoutLogin(); setLoginPassword(''); setLoginModalOpen(false); await refreshStatus(); await refreshLogin(); await refreshLogs(); }, '已清除内网账号', '清除内网账号失败')} onOpenInnerAdmin={() => run(agentApi.openInnerAdmin)} onRemoteExecutionChange={(next, confirmed) => run(async () => { await agentApi.saveRemoteExecutionSettings(next, confirmed); await refreshRemoteExecutionSettings(); await refreshLogs(); }, next.enabled ? '远程任务设置已更新' : '已关闭远程任务', '远程任务设置更新失败')} onRemoteClientsChange={setRemoteClients} onRuleChange={(requestType, mode) => run(async () => { await agentApi.setRule(requestType, mode); await refreshSettings(); }, '审批规则已更新', '审批规则更新失败')} onTimeoutChange={seconds => run(async () => { await agentApi.setTimeout(seconds); await refreshSettings(); }, '审批超时已更新', '审批超时更新失败')} onAutoStartChange={enabled => run(async () => { const result = await agentApi.setAutoStart(enabled); await refreshSettings(); await refreshLogs(); notify('success', result.auto_start ? '已启用开机自启' : '已关闭开机自启'); }, undefined, '开机自启更新失败')} onUnityEditorSettingsChange={editors => setSettings(current => current ? { ...current, editors } : current)} svnConnections={svnConnections} svnModalOpen={svnModalOpen} svnDraft={svnDraft} onOpenSvnModal={() => setSvnModalOpen(true)} onCloseSvnModal={() => setSvnModalOpen(false)} onSvnDraftChange={setSvnDraft} onSaveSvnConnection={() => run(async () => { await agentApi.saveSvnConnection(svnDraft); setSvnModalOpen(false); await refreshSvnConnections(); }, 'SVN 账号已保存', '保存 SVN 账号失败')} onTestSvnConnection={testSvnConnection} svnTesting={svnTesting} onRemoveSvnConnection={() => run(async () => { await agentApi.removeSvnConnection(); await refreshSvnConnections(); }, 'SVN 账号已删除', '删除 SVN 账号失败')} updateStatus={updateStatus} updateBusy={updateBusy} onCheckUpdate={() => runUpdateOperation(agentApi.checkUpdate, result => result.available_version ? `发现新版本 v${result.available_version}` : '当前已是最新版本')} onDownloadUpdate={() => runUpdateOperation(agentApi.downloadUpdate, result => `v${result.available_version} 更新已下载`)} onCancelUpdateDownload={cancelUpdateDownload} onInstallUpdate={() => runUpdateOperation(agentApi.installUpdate)} onUpdatePreferences={(autoCheck, autoDownload) => runUpdateOperation(() => agentApi.setUpdatePreferences(autoCheck, autoDownload))} />;
    return <LogsPage logs={logs} onRefresh={() => run(refreshLogs)} onExport={() => run(async () => {
      const result = await agentApi.exportDiagnostics();
      if (!result.canceled) notify('success', `诊断包已导出：${result.path || ''}`);
    }, undefined, '导出诊断包失败')} />;
  })();

  return (
    <Shell
      currentPage={page}
      approvalCount={approvals.length}
      identity={dashboardIdentity}
      dashboardEnabled={dashboardEnabled()}
      agentVersion={status?.version || '--'}
      updateBusy={updateBusy}
      currentTask={status?.current_task || null}
      onLoadTaskHistory={agentApi.taskHistory}
      onNavigate={setPage}
      onOpenDashboard={() => run(agentApi.openDashboard)}
      onOpenBuiltinAi={() => { void openBuiltinAi(); }}
      onCheckUpdate={() => runUpdateOperation(agentApi.checkUpdate, result => result.available_version ? `发现新版本 v${result.available_version}` : '当前已是最新版本')}
      onOpenAgentDirectory={() => run(agentApi.openAgentDirectory, 'Agent 文件夹已打开', '打开 Agent 文件夹失败')}
      onQuit={() => { void agentApi.quitAgent(); }}
    >
      <NotificationCenter messages={messages} onClose={dismissNotification} />
      <div className={`builtin-ai-page-host ${page === 'builtin-ai' ? 'active' : 'inactive'}`} aria-hidden={page !== 'builtin-ai'}>
        {builtinAiActivated ? builtinAiContent : null}
      </div>
      {page !== 'builtin-ai' ? content : null}
    </Shell>
  );
}

function SettingsLoadState({ loading, error, onRetry }: { loading: boolean; error: string; onRetry: () => void }) {
  return (
    <>
      <PageHeader title="设置" description="管理任务权限、账号、工具与启动设置。" />
      {loading
        ? <div className="page-loading"><span className="spinner" />正在读取 Agent 配置</div>
        : <div className="blocker account-blocker" role="alert">
            <ShieldAlert size={18} />
            <div><strong>Agent 配置读取失败</strong><span>{error || '部分配置暂时不可用，请重新读取。'}</span></div>
            <button type="button" className="btn" onClick={onRetry}><RefreshCw size={15} />重新读取</button>
          </div>}
    </>
  );
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

createRoot(document.getElementById('root')!).render(<App />);
