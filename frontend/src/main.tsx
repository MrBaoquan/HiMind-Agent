import { useEffect, useState } from 'react';
import { createRoot } from 'react-dom/client';
import '../styles.css';
import { NotificationCenter } from './components/Common';
import { Shell } from './components/Shell';
import { ApprovalsPage } from './pages/ApprovalsPage';
import { DashboardPage } from './pages/DashboardPage';
import { LogsPage } from './pages/LogsPage';
import { PluginsPage } from './pages/PluginsPage';
import { SettingsPage } from './pages/SettingsPage';
import { agentApi, type AgentStatus, type ApprovalItem, type ApprovalSettings, type CapabilityItem, type LogItem, type LoginState, type PluginRegistry } from './services/agentApi';
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
  const [capabilities, setCapabilities] = useState<CapabilityItem[]>([]);
  const [messages, setMessages] = useState<UiMessage[]>([]);
  const [loginModalOpen, setLoginModalOpen] = useState(false);
  const [loginUsername, setLoginUsername] = useState('');
  const [loginPassword, setLoginPassword] = useState('');

  async function refreshStatus() { setStatus(await agentApi.status()); }
  async function refreshApprovals() { setApprovals(await agentApi.approvals()); }
  async function refreshSettings() { setSettings(await agentApi.settings()); }
  async function refreshLogin() { setLoginState(await agentApi.login()); }
  async function refreshLogs() { setLogs(await agentApi.logs()); }
  async function refreshPlugins() {
    const [registry, capabilityItems] = await Promise.all([agentApi.plugins(), agentApi.capabilities()]);
    setPluginRegistry(registry);
    setCapabilities(Array.isArray(capabilityItems) ? capabilityItems : []);
  }

  async function refreshAll() {
    await refreshStatus();
    await refreshApprovals();
    await refreshSettings();
    await refreshLogin();
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

  function openLoginModal() {
    setLoginUsername(current => current || loginState?.account || '');
    setLoginPassword('');
    setLoginModalOpen(true);
  }

  const content = (() => {
    if (page === 'dashboard') return <DashboardPage status={status} approvals={approvals} settings={settings} loginState={loginState} onOpenDashboard={() => run(agentApi.openDashboard)} onOpenAgentDirectory={() => run(agentApi.openAgentDirectory)} onOpenSettings={() => setPage('settings')} />;
    if (page === 'approvals') return <ApprovalsPage approvals={approvals} onRefresh={() => run(refreshApprovals)} onRespond={(id, approved) => run(async () => { await agentApi.respondApproval(id, approved); await refreshApprovals(); await refreshStatus(); }, undefined, '审批处理失败')} />;
    if (page === 'plugins') return <PluginsPage registry={pluginRegistry} capabilities={capabilities} onRefresh={() => run(refreshPlugins)} onOpenDirectory={() => run(agentApi.openPluginDirectory)} onOpenView={(pluginId, viewId) => run(() => agentApi.openPluginView(pluginId, viewId), '插件窗口已打开', '打开插件窗口失败')} onCreateShortcut={(pluginId, viewId, title) => run(() => agentApi.createPluginViewShortcut(pluginId, viewId, title), '桌面快捷方式已创建', '创建桌面快捷方式失败')} />;
    if (page === 'settings') return <SettingsPage settings={settings} loginState={loginState} loginModalOpen={loginModalOpen} loginUsername={loginUsername} loginPassword={loginPassword} onOpenLoginModal={openLoginModal} onCloseLoginModal={() => setLoginModalOpen(false)} onUsernameChange={setLoginUsername} onPasswordChange={setLoginPassword} onSaveLogin={() => run(async () => { await agentApi.saveLogin(loginUsername, loginPassword); setLoginPassword(''); setLoginModalOpen(false); await refreshStatus(); await refreshLogin(); await refreshLogs(); }, '内网账号已保存到当前 Agent', '保存内网账号失败')} onLogoutLogin={() => run(async () => { await agentApi.logoutLogin(); setLoginPassword(''); setLoginModalOpen(false); await refreshStatus(); await refreshLogin(); await refreshLogs(); }, '已清除当前 Agent 保存的内网凭据', '清除内网凭据失败')} onOpenInnerAdmin={() => run(agentApi.openInnerAdmin)} onRuleChange={(requestType, mode) => run(async () => { await agentApi.setRule(requestType, mode); await refreshSettings(); }, '审批规则已更新', '审批规则更新失败')} onTimeoutChange={seconds => run(async () => { await agentApi.setTimeout(seconds); await refreshSettings(); }, '审批超时已更新', '审批超时更新失败')} onAutoStartChange={enabled => run(async () => { const result = await agentApi.setAutoStart(enabled); await refreshSettings(); await refreshLogs(); notify('success', result.auto_start ? '已启用开机自启' : '已关闭开机自启'); }, undefined, '开机自启更新失败')} />;
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
