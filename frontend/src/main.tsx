import { useEffect, useState } from 'react';
import { createRoot } from 'react-dom/client';
import '../styles.css';
import { NotificationCenter } from './components/Common';
import { Shell } from './components/Shell';
import { ApprovalsPage } from './pages/ApprovalsPage';
import { DashboardPage } from './pages/DashboardPage';
import { LogsPage } from './pages/LogsPage';
import { PluginsPage } from './pages/PluginsPage';
import { SkillsWorkspacePage } from './pages/SkillsWorkspacePage';
import { SettingsPage } from './pages/SettingsPage';
import { agentApi, type AgentStatus, type ApprovalItem, type ApprovalSettings, type CapabilityItem, type CodexSkillStatusResponse, type SkillCatalogResponse, type OrganizationSkillCatalogItem, type AuthoringSkillDraft, type SkillSubmissionStatus, type LogItem, type LoginState, type PluginRegistry, type SvnConnection, type SvnConnectionInput } from './services/agentApi';
import { formatError, type PageKey, type UiMessage } from './types';

let nextNotificationId = 1;

function App() {
  const [page, setPage] = useState<PageKey>('dashboard');
  const [status, setStatus] = useState<AgentStatus | null>(null);
  const [approvals, setApprovals] = useState<ApprovalItem[]>([]);
  const [settings, setSettings] = useState<ApprovalSettings | null>(null);
  const [loginState, setLoginState] = useState<LoginState | null>(null);
  const [logs, setLogs] = useState<LogItem[]>([]);
  const [pluginRegistry, setPluginRegistry] = useState<PluginRegistry | null>(null);
  const [pluginsLoading, setPluginsLoading] = useState(true);
  const [capabilities, setCapabilities] = useState<CapabilityItem[]>([]);
  const [pluginCatalog, setPluginCatalog] = useState<import('./services/agentApi').PluginCatalogItem[]>([]);
  const [skillCatalog, setSkillCatalog] = useState<SkillCatalogResponse | null>(null);
  const [skillStatus, setSkillStatus] = useState<CodexSkillStatusResponse | null>(null);
  const [organizationSkills, setOrganizationSkills] = useState<OrganizationSkillCatalogItem[]>([]);
  const [skillMarketError, setSkillMarketError] = useState<string | null>(null);
  const [skillDrafts, setSkillDrafts] = useState<AuthoringSkillDraft[]>([]);
  const [skillSubmissions, setSkillSubmissions] = useState<SkillSubmissionStatus[]>([]);
  const [skillError, setSkillError] = useState<string | null>(null);
  const [skillOperation, setSkillOperation] = useState<string | null>(null);
  const [messages, setMessages] = useState<UiMessage[]>([]);
  const [loginModalOpen, setLoginModalOpen] = useState(false);
  const [loginUsername, setLoginUsername] = useState('');
  const [loginPassword, setLoginPassword] = useState('');
  const [svnConnections, setSvnConnections] = useState<SvnConnection[]>([]);
  const [svnModalOpen, setSvnModalOpen] = useState(false);
  const [svnDraft, setSvnDraft] = useState<SvnConnectionInput>({ username: '', password: '' });

  async function refreshStatus() { setStatus(await agentApi.status()); }
  async function refreshApprovals() { setApprovals(await agentApi.approvals()); }
  async function refreshSettings() { setSettings(await agentApi.settings()); }
  async function refreshLogin() { setLoginState(await agentApi.login()); }
  async function refreshLogs() { setLogs(await agentApi.logs()); }
  async function refreshSvnConnections() { setSvnConnections((await agentApi.svnConnections()).items || []); }
  async function refreshPlugins() {
    setPluginsLoading(true);
    try {
      const [registry, capabilityItems] = await Promise.all([agentApi.plugins(), agentApi.capabilities()]);
      setPluginRegistry(registry);
      setCapabilities(Array.isArray(capabilityItems) ? capabilityItems : []);
      try {
        const catalog = await agentApi.pluginCatalog();
        setPluginCatalog(Array.isArray(catalog) ? catalog : []);
      } catch (error) {
        setPluginCatalog([]);
        console.error('Plugin catalog unavailable', error);
      }
      await refreshSkills();
    } finally {
      setPluginsLoading(false);
    }
  }

  async function refreshSkills() {
    const [catalogResult, statusResult, marketResult, draftsResult, submissionsResult] = await Promise.allSettled([
      agentApi.skillCatalog(),
      agentApi.codexSkillStatus(),
      agentApi.organizationSkillCatalog(),
      agentApi.skillDrafts(),
      agentApi.skillSubmissions(),
    ]);
    const errors: string[] = [];
    if (catalogResult.status === 'fulfilled') {
      setSkillCatalog(catalogResult.value);
    } else {
      setSkillCatalog(null);
      errors.push(formatError(catalogResult.reason, 'Skill Store 读取失败'));
    }
    if (statusResult.status === 'fulfilled') {
      setSkillStatus(statusResult.value);
    } else {
      setSkillStatus(null);
      errors.push(formatError(statusResult.reason, 'Codex 目标读取失败'));
    }
	if (marketResult.status === 'fulfilled') {
	  setOrganizationSkills(Array.isArray(marketResult.value) ? marketResult.value : []);
	  setSkillMarketError(null);
	} else {
	  setOrganizationSkills([]);
	  setSkillMarketError(formatError(marketResult.reason, '组织商城暂不可用'));
	}
    if (draftsResult.status === 'fulfilled') setSkillDrafts(draftsResult.value || []);
    else errors.push(formatError(draftsResult.reason, '本地 Skill 草稿读取失败'));
    if (submissionsResult.status === 'fulfilled') setSkillSubmissions(submissionsResult.value || []);
    else setSkillSubmissions([]);
    setSkillError(errors.length ? errors.join('；') : null);
  }

  async function refreshAll() {
    await refreshStatus();
    await refreshApprovals();
    await refreshSettings();
    await refreshLogin();
    await refreshSvnConnections();
    await refreshPlugins();
    await refreshLogs();
  }

  useEffect(() => {
    refreshAll().catch(error => notify('error', formatError(error, 'Agent 面板初始化失败')));
    const timer = window.setInterval(() => {
      Promise.all([refreshStatus(), refreshApprovals(), refreshLogin()]).catch(console.error);
    }, 5000);
    return () => window.clearInterval(timer);
  }, []);

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

  function openLoginModal() {
    setLoginUsername(current => current || loginState?.account || '');
    setLoginPassword('');
    setLoginModalOpen(true);
  }

  const content = (() => {
    if (page === 'dashboard') return <DashboardPage status={status} approvals={approvals} settings={settings} loginState={loginState} onOpenDashboard={() => run(agentApi.openDashboard)} onOpenAgentDirectory={() => run(agentApi.openAgentDirectory)} onOpenSettings={() => setPage('settings')} />;
    if (page === 'approvals') return <ApprovalsPage approvals={approvals} onRefresh={() => run(refreshApprovals)} onRespond={(id, approved) => run(async () => { await agentApi.respondApproval(id, approved); await refreshApprovals(); await refreshStatus(); }, undefined, '审批处理失败')} />;
      if (page === 'plugins') return <PluginsPage loading={pluginsLoading} registry={pluginRegistry} catalog={pluginCatalog} capabilities={capabilities} onRefresh={() => run(refreshPlugins)} onInstall={(pluginId) => run(async () => { await agentApi.installPlugin(pluginId); await refreshPlugins(); }, '插件已安装', '安装插件失败')} onUninstall={(pluginId) => run(async () => { await agentApi.uninstallPlugin(pluginId); await refreshPlugins(); }, '插件已卸载', '卸载插件失败')} onRollback={(pluginId) => run(async () => { await agentApi.rollbackPlugin(pluginId); await refreshPlugins(); }, '插件已回滚', '插件回滚失败')} onSetEnabled={(pluginId, enabled) => run(async () => { await agentApi.setPluginEnabled(pluginId, enabled); await refreshPlugins(); }, enabled ? '插件已启用' : '插件已停用', enabled ? '启用插件失败' : '停用插件失败')} onOpenDirectory={() => run(agentApi.openPluginDirectory)} onRegisterDevelopment={() => run(async () => { const pluginId = await agentApi.registerDevelopmentPlugin(); await refreshPlugins(); notify('success', `已加载开发插件：${pluginId}`); }, undefined, '加载本地插件工程失败')} onUnregisterDevelopment={(pluginId) => run(async () => { await agentApi.unregisterDevelopmentPlugin(pluginId); await refreshPlugins(); }, '已移除开发插件注册', '移除开发插件失败')} onInvokeDevelopment={agentApi.invokeDevelopmentPlugin} onOpenView={(pluginId, viewId) => run(() => agentApi.openPluginView(pluginId, viewId), '插件窗口已打开', '打开插件窗口失败')} onCreateShortcut={(pluginId, viewId, title) => run(() => agentApi.createPluginViewShortcut(pluginId, viewId, title), '桌面快捷方式已创建', '创建桌面快捷方式失败')} />;
    if (page === 'skills') return <SkillsWorkspacePage
      catalog={skillCatalog}
      status={skillStatus}
      error={skillError}
      marketplace={organizationSkills}
      marketplaceError={skillMarketError}
      drafts={skillDrafts}
      submissions={skillSubmissions}
      busyAction={skillOperation}
      onRefresh={() => run(refreshSkills)}
      onSyncAll={() => runSkillOperation('sync-all', async () => {
        const result = await agentApi.syncCodexSkills();
        await refreshSkills();
        return `同步完成：${result.rendered.length} 个成功，${result.skipped.length} 个跳过，${result.blocked.length} 个阻止`;
      }, '同步 Codex Skill 失败')}
      onSyncSkill={(skillId) => runSkillOperation(`sync:${skillId}`, async () => {
        const result = await agentApi.syncCodexSkill(skillId);
        await refreshSkills();
        return result.rendered.state === 'skipped' ? 'Skill 已是最新版本' : `已安装 ${result.rendered.skill_id}`;
      }, '安装 Skill 失败')}
      onPlanMarketplace={agentApi.planOrganizationSkillInstall}
      onInstallMarketplace={(skillId, optionalPluginIds) => runSkillOperation(`market:${skillId}`, async () => {
        const result = await agentApi.installOrganizationSkill(skillId, optionalPluginIds);
        await refreshSkills();
        return `已从组织商城安装 ${result.record.manifest.name} v${result.record.manifest.version}`;
      }, '商城 Skill 安装失败')}
      onSaveDraft={async (input) => { const result = await agentApi.saveSkillDraft(input); await refreshSkills(); return result; }}
      onTestDraft={(skillId, version) => runSkillOperation(`test:${skillId}`, async () => { await agentApi.testSkillDraft(skillId, version); await refreshSkills(); return '已部署到 Codex，请完成实际对话测试'; }, 'Skill 本地测试失败')}
      onConfirmDraft={(skillId, version) => runSkillOperation(`confirm:${skillId}`, async () => { await agentApi.confirmSkillDraft(skillId, version); await refreshSkills(); return '已确认本地测试通过'; }, '确认测试失败')}
      onSubmitDraft={(skillId, version) => runSkillOperation(`submit:${skillId}`, async () => { await agentApi.submitSkillDraft(skillId, version); await refreshSkills(); return '已提交 Dashboard 审核'; }, '提交 Skill 审核失败')}
      onRepair={(skillId) => runSkillOperation(`repair:${skillId}`, async () => {
        const result = await agentApi.repairCodexSkill(skillId, true);
        await refreshSkills();
        return result.backup_root ? 'Skill 已修复，原修改已保留为备份' : 'Skill 已重新同步';
      }, '修复 Skill 失败')}
      onUninstall={(skillId) => runSkillOperation(`uninstall:${skillId}`, async () => {
        const result = await agentApi.uninstallCodexSkill(skillId);
        await refreshSkills();
        return result.removed.removed ? `已卸载 ${result.removed.skill_id}` : `未卸载 ${result.removed.skill_id}`;
      }, '卸载 Codex Skill 失败')}
      onOpenDirectory={(path) => run(() => agentApi.openFolder(path), '目录已打开', '打开目录失败')}
    />;
    if (page === 'settings') return <SettingsPage settings={settings} loginState={loginState} loginModalOpen={loginModalOpen} loginUsername={loginUsername} loginPassword={loginPassword} onOpenLoginModal={openLoginModal} onCloseLoginModal={() => setLoginModalOpen(false)} onUsernameChange={setLoginUsername} onPasswordChange={setLoginPassword} onSaveLogin={() => run(async () => { await agentApi.saveLogin(loginUsername, loginPassword); setLoginPassword(''); setLoginModalOpen(false); await refreshStatus(); await refreshLogin(); await refreshLogs(); }, '内网账号已保存到当前 Agent', '保存内网账号失败')} onLogoutLogin={() => run(async () => { await agentApi.logoutLogin(); setLoginPassword(''); setLoginModalOpen(false); await refreshStatus(); await refreshLogin(); await refreshLogs(); }, '已清除当前 Agent 保存的内网凭据', '清除内网凭据失败')} onOpenInnerAdmin={() => run(agentApi.openInnerAdmin)} onRuleChange={(requestType, mode) => run(async () => { await agentApi.setRule(requestType, mode); await refreshSettings(); }, '审批规则已更新', '审批规则更新失败')} onTimeoutChange={seconds => run(async () => { await agentApi.setTimeout(seconds); await refreshSettings(); }, '审批超时已更新', '审批超时更新失败')} onAutoStartChange={enabled => run(async () => { const result = await agentApi.setAutoStart(enabled); await refreshSettings(); await refreshLogs(); notify('success', result.auto_start ? '已启用开机自启' : '已关闭开机自启'); }, undefined, '开机自启更新失败')} svnConnections={svnConnections} svnModalOpen={svnModalOpen} svnDraft={svnDraft} onOpenSvnModal={() => setSvnModalOpen(true)} onCloseSvnModal={() => setSvnModalOpen(false)} onSvnDraftChange={setSvnDraft} onSaveSvnConnection={() => run(async () => { await agentApi.saveSvnConnection(svnDraft); setSvnModalOpen(false); await refreshSvnConnections(); }, 'SVN 账号已保存', '保存 SVN 账号失败')} onTestSvnConnection={() => run(agentApi.testSvnConnection, undefined, '测试 SVN 连接失败')} onRemoveSvnConnection={() => run(async () => { await agentApi.removeSvnConnection(); await refreshSvnConnections(); }, 'SVN 账号已删除', '删除 SVN 账号失败')} />;
    return <LogsPage logs={logs} onRefresh={() => run(refreshLogs)} />;
  })();

  return (
    <Shell version={status?.version} currentPage={page} approvalCount={approvals.length} onNavigate={setPage}>
      <NotificationCenter messages={messages} onClose={dismissNotification} />
      {content}
    </Shell>
  );
}

createRoot(document.getElementById('root')!).render(<App />);
