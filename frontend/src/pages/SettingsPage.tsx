import { useEffect, useRef, useState, type ReactNode } from 'react';
import { agentApi, type AgentModeSettings, type AgentUpdateStatus, type ApprovalSettings, type BuiltinAIRuntimeInstallationStatus, type BuiltinAIRuntimeStatus, type LoginState, type RemoteClientOverview, type RemoteClientStatus, type RemoteClientVendor, type RemoteExecutionSettings, type SvnConnection, type SvnConnectionInput, type UnityEditorSettings } from '../services/agentApi';
import { Bell, BellOff, Bot, CheckCircle2, ChevronDown, Clock3, Database, Download, ExternalLink, FolderOpen, Globe2, Inbox, KeyRound, LoaderCircle, LockKeyhole, Monitor, MoreHorizontal, PencilLine, Power, RefreshCw, RotateCcw, Save, Search, ShieldAlert, ShieldCheck, ShieldX, Trash2, Wrench, X } from 'lucide-react';
import { IconButton, PageHeader, Pill } from '../components/Common';

type SettingsSection = 'remote' | 'approval' | 'remote-tools' | 'accounts' | 'tools' | 'general';

const SETTINGS_SECTIONS = [
  { key: 'remote', label: '远程任务', description: '接收与执行', icon: ShieldCheck },
  { key: 'approval', label: '审批策略', description: '确认与授权', icon: ShieldAlert },
  { key: 'remote-tools', label: '远控工具', description: '路径配置', icon: Monitor },
  { key: 'accounts', label: '账号', description: '内网和 SVN', icon: KeyRound },
  { key: 'tools', label: '工具', description: '本机编辑器', icon: Wrench },
  { key: 'general', label: '通用', description: '启动与更新', icon: Power },
] satisfies { key: SettingsSection; label: string; description: string; icon: typeof ShieldCheck }[];

const BUILTIN_APPROVAL_RULES = new Set(['remote_connect', 'upload_code', 'upload_placeholder', 'controlled_operation', '*', 'risk:R1', 'risk:R2', 'risk:R3', 'risk:R4']);

type ApprovalProfile = 'strict' | 'balanced' | 'relaxed' | 'trusted' | 'silent_deny';
type ApprovalRuleMode = 'inherit' | 'manual' | 'auto_approve' | 'auto_deny';

function agentModeLabel(mode?: string) {
  if (mode === 'independent') return '独立模式';
  if (mode === 'connected') return '组织模式';
  return '未确定';
}

const APPROVAL_PROFILE_OPTIONS = [
  { value: 'strict', label: '更安全', description: '除单独授权外，受控操作都先确认', icon: ShieldAlert },
  { value: 'balanced', label: '推荐', description: '查询预览自动执行，修改操作先确认', icon: ShieldCheck },
  { value: 'relaxed', label: '少打扰', description: '查询和普通修改自动执行', icon: BellOff },
  { value: 'trusted', label: '完全信任', description: '删除、发布等高风险操作也自动执行', icon: KeyRound },
  { value: 'silent_deny', label: '只执行已授权项', description: '其他受控请求直接拒绝，不弹审批', icon: ShieldX },
] satisfies ChoiceOption[];

const APPROVAL_NOTIFICATION_OPTIONS = [
  { value: 'popup', label: '右下角提醒', icon: Bell },
  { value: 'tray', label: '仅托盘提示', icon: Monitor },
  { value: 'inbox', label: '仅审批中心', icon: Inbox },
] satisfies ChoiceOption[];

const APPROVAL_RULE_OPTIONS = [
  { value: 'inherit', label: '跟随整体' },
  { value: 'manual', label: '每次确认' },
  { value: 'auto_approve', label: '自动允许' },
  { value: 'auto_deny', label: '自动拒绝' },
] satisfies ChoiceOption[];

const APPROVAL_CREATE_RULE_OPTIONS = APPROVAL_RULE_OPTIONS.filter(option => option.value !== 'inherit');

export function SettingsPage({
  settings,
  remoteExecutionSettings,
  remoteClients,
  loginState,
  loginModalOpen,
  loginUsername,
  loginPassword,
  onOpenLoginModal,
  onCloseLoginModal,
  onUsernameChange,
  onPasswordChange,
  onSaveLogin,
  onLogoutLogin,
  onOpenInnerAdmin,
  onRemoteExecutionChange,
  onRemoteClientsChange,
  onRuleChange,
  onApprovalProfileChange,
  onApprovalNotificationModeChange,
  onTimeoutChange,
  onAutoStartChange,
  onUnityEditorSettingsChange,
  svnConnections,
  svnModalOpen,
  svnDraft,
  onOpenSvnModal,
  onCloseSvnModal,
  onSvnDraftChange,
  onSaveSvnConnection,
  onTestSvnConnection,
  svnTesting,
  onRemoveSvnConnection,
  updateStatus,
  updateBusy,
  onCheckUpdate,
  onDownloadUpdate,
  onCancelUpdateDownload,
  onInstallUpdate,
  onUpdatePreferences,
  independentMode = false,
  initialSection = 'remote',
}: {
  settings: ApprovalSettings | null;
  remoteExecutionSettings: RemoteExecutionSettings | null;
  remoteClients: RemoteClientOverview | null;
  loginState: LoginState | null;
  loginModalOpen: boolean;
  loginUsername: string;
  loginPassword: string;
  onOpenLoginModal: () => void;
  onCloseLoginModal: () => void;
  onUsernameChange: (value: string) => void;
  onPasswordChange: (value: string) => void;
  onSaveLogin: () => void;
  onLogoutLogin: () => void;
  onOpenInnerAdmin: () => void;
  onRemoteExecutionChange: (settings: RemoteExecutionSettings, fullAccessConfirmed?: boolean) => void;
  onRemoteClientsChange: (overview: RemoteClientOverview) => void;
  onRuleChange: (requestType: string, mode: string) => void;
  onApprovalProfileChange: (profile: string, confirmed?: boolean) => void;
  onApprovalNotificationModeChange: (mode: string) => void;
  onTimeoutChange: (seconds: number) => void;
  onAutoStartChange: (enabled: boolean) => void;
  onUnityEditorSettingsChange: (settings: UnityEditorSettings) => void;
  svnConnections: SvnConnection[];
  svnModalOpen: boolean;
  svnDraft: SvnConnectionInput;
  onOpenSvnModal: () => void;
  onCloseSvnModal: () => void;
  onSvnDraftChange: (draft: SvnConnectionInput) => void;
  onSaveSvnConnection: () => void;
  onTestSvnConnection: () => void;
  svnTesting: boolean;
  onRemoveSvnConnection: () => void;
  updateStatus: AgentUpdateStatus | null;
  updateBusy: boolean;
  onCheckUpdate: () => void;
  onDownloadUpdate: () => void;
  onCancelUpdateDownload: () => void;
  onInstallUpdate: () => void;
  onUpdatePreferences: (autoCheck: boolean, autoDownload: boolean) => void;
  independentMode?: boolean;
  initialSection?: SettingsSection;
}) {
  const [builtinAIRuntimeStatus, setBuiltinAIRuntimeStatus] = useState<BuiltinAIRuntimeStatus | null>(null);
  const [builtinAIRuntimeInstallation, setBuiltinAIRuntimeInstallation] = useState<BuiltinAIRuntimeInstallationStatus | null>(null);
  const [builtinAIRuntimeBusy, setBuiltinAIRuntimeBusy] = useState(false);
  const [builtinAIRuntimeCheckBusy, setBuiltinAIRuntimeCheckBusy] = useState(false);
  const [builtinAIRuntimeFeedback, setBuiltinAIRuntimeFeedback] = useState('');
  const [pendingRuntimeUninstall, setPendingRuntimeUninstall] = useState(false);
  const [approvalCapabilityId, setApprovalCapabilityId] = useState('');
  const [approvalCapabilityMode, setApprovalCapabilityMode] = useState<Exclude<ApprovalRuleMode, 'inherit'>>('manual');
  const exactApprovalRules = Object.entries(settings?.rules || {}).filter(([requestType]) => !BUILTIN_APPROVAL_RULES.has(requestType));
  const runtimeWorking = builtinAIRuntimeInstallation?.state === 'working';
  const runtimeReady = builtinAIRuntimeStatus?.status === 'ready';
  useEffect(() => {
    let disposed = false;
    const load = async () => {
      try {
        const installation = await agentApi.builtinAiRuntimeInstallationStatus();
        if (disposed) return;
        setBuiltinAIRuntimeInstallation(installation);
        setBuiltinAIRuntimeStatus(installation.runtime);
        if (installation.runtime.status === 'ready') {
          try {
            const checked = await agentApi.checkBuiltinAiRuntimeUpdate();
            if (!disposed) {
              setBuiltinAIRuntimeInstallation(checked);
              setBuiltinAIRuntimeStatus(checked.runtime);
            }
          } catch {
            // A status page should remain usable when Dashboard is offline.
          }
        }
      } catch {
        if (!disposed) setBuiltinAIRuntimeStatus(null);
      }
    };
    void load();
    return () => { disposed = true; };
  }, []);
  useEffect(() => {
    if (!runtimeWorking) return;
    const timer = window.setInterval(async () => {
      try {
        const next = await agentApi.builtinAiRuntimeInstallationStatus();
        setBuiltinAIRuntimeInstallation(next);
        setBuiltinAIRuntimeStatus(next.runtime);
        if (next.state === 'ready' || next.state === 'idle') setBuiltinAIRuntimeFeedback(next.message);
        if (next.state === 'failed') setBuiltinAIRuntimeFeedback(next.error || 'HiMind AI 运行时安装失败，请重试');
      } catch {
        setBuiltinAIRuntimeFeedback('暂时无法读取安装进度，请稍后重试');
      }
    }, 700);
    return () => window.clearInterval(timer);
  }, [runtimeWorking]);
  const refreshBuiltinAIRuntime = async () => {
    setBuiltinAIRuntimeFeedback('');
    try {
      const installation = await agentApi.builtinAiRuntimeInstallationStatus();
      setBuiltinAIRuntimeInstallation(installation);
      setBuiltinAIRuntimeStatus(installation.runtime);
      if (installation.runtime.status === 'ready') await checkBuiltinAIRuntimeUpdate();
    } catch {
      setBuiltinAIRuntimeFeedback('暂时无法检查 HiMind AI 运行时，请稍后重试');
    }
  };
  const checkBuiltinAIRuntimeUpdate = async () => {
    if (builtinAIRuntimeCheckBusy || runtimeWorking) return;
    setBuiltinAIRuntimeCheckBusy(true);
    setBuiltinAIRuntimeFeedback('正在检查 HiMind AI 运行时更新');
    try {
      const installation = await agentApi.checkBuiltinAiRuntimeUpdate();
      setBuiltinAIRuntimeInstallation(installation);
      setBuiltinAIRuntimeStatus(installation.runtime);
      setBuiltinAIRuntimeFeedback(installation.message);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error || '');
      setBuiltinAIRuntimeFeedback(message.includes('尚未安装') ? '请先安装 HiMind AI 运行时' : '暂时无法检查更新，请稍后重试');
    } finally {
      setBuiltinAIRuntimeCheckBusy(false);
    }
  };
  const startBuiltinAIRuntimeOperation = async (operation: BuiltinAIRuntimeInstallationStatus['operation']) => {
    if (builtinAIRuntimeBusy || runtimeWorking) return;
    setBuiltinAIRuntimeBusy(true);
    setBuiltinAIRuntimeFeedback('');
    try {
      const installation = await agentApi.startBuiltinAiRuntimeInstall(operation);
      setBuiltinAIRuntimeInstallation(installation);
      setBuiltinAIRuntimeStatus(installation.runtime);
      setBuiltinAIRuntimeFeedback(installation.message);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error || '');
      setBuiltinAIRuntimeFeedback(message.includes('没有可用') || message.includes('发布')
        ? '当前没有可用的 HiMind AI 运行时安装包'
        : `HiMind AI 运行时${runtimeActionLabel(operation)}失败，请稍后重试`);
    } finally {
      setBuiltinAIRuntimeBusy(false);
    }
  };
  const primaryRuntimeAction = async () => {
    if (!runtimeReady) return startBuiltinAIRuntimeOperation('install');
    if (builtinAIRuntimeInstallation?.update_available) return startBuiltinAIRuntimeOperation('update');
    return checkBuiltinAIRuntimeUpdate();
  };
  const [unityEditorPath, setUnityEditorPath] = useState('');
  const [unityEditorSettings, setUnityEditorSettings] = useState<UnityEditorSettings | null>(null);
  const [editorFeedback, setEditorFeedback] = useState('');
  const [editorSaving, setEditorSaving] = useState(false);
  const [pendingFullAccess, setPendingFullAccess] = useState<RemoteExecutionSettings | null>(null);
  const [pendingApprovalTrust, setPendingApprovalTrust] = useState(false);
  const [agentMode, setAgentMode] = useState<AgentModeSettings | null>(null);
  const [agentModeBusy, setAgentModeBusy] = useState(false);
  const [agentModeFeedback, setAgentModeFeedback] = useState('');
  const [remoteClientDrafts, setRemoteClientDrafts] = useState<Record<RemoteClientVendor, string>>({ sunlogin: '', todesk: '' });
  const remoteClientDraftsInitialized = useRef(false);
  const [remoteClientBusy, setRemoteClientBusy] = useState<RemoteClientVendor | 'detect' | null>(null);
  const [remoteClientFeedback, setRemoteClientFeedback] = useState<Record<RemoteClientVendor, string>>({ sunlogin: '', todesk: '' });
  const [section, setSection] = useState<SettingsSection>(initialSection);
  useEffect(() => setSection(initialSection), [initialSection]);
  useEffect(() => {
    setUnityEditorSettings(settings?.editors || null);
    setUnityEditorPath(settings?.editors?.unity_editor_path || '');
  }, [settings?.editors]);
  useEffect(() => {
    if (!remoteClients) return;
    const nextDrafts = remoteClientDraftsFromOverview(remoteClients);
    setRemoteClientDrafts(current => {
      if (!remoteClientDraftsInitialized.current) {
        remoteClientDraftsInitialized.current = true;
        return { ...current, ...nextDrafts };
      }
      return REMOTE_CLIENT_OPTIONS.reduce((drafts, option) => {
        const persistedPath = remoteClients.items.find(item => item.vendor === option.vendor)?.configured_path || '';
        const localPath = current[option.vendor] || '';
        return { ...drafts, [option.vendor]: localPath === persistedPath ? nextDrafts[option.vendor] : localPath };
      }, { ...current });
    });
  }, [remoteClients]);
  useEffect(() => {
    let active = true;
    void agentApi.agentMode().then(value => { if (active) setAgentMode(value); }).catch(error => {
      if (active) setAgentModeFeedback(error instanceof Error ? error.message : '运行模式读取失败');
    });
    return () => { active = false; };
  }, []);

  async function changeAgentMode(mode: 'connected' | 'independent') {
    if (!agentMode || agentMode.mode === mode || agentModeBusy) return;
    const title = mode === 'independent' ? '切换到独立模式？' : '切换到组织模式？';
    const message = mode === 'independent'
      ? '保留本机 AI、技能、插件、MCP 和工程能力，不参与组织调度。保存后需重启 Agent。'
      : '接入 HiMind 工作台，启用组织调度、共享服务和审计。保存后需重启 Agent。';
    if (!window.confirm(`${title}\n\n${message}`)) return;
    setAgentModeBusy(true);
    setAgentModeFeedback('');
    try {
      const next = await agentApi.setAgentMode(mode);
      setAgentMode(next);
      setAgentModeFeedback('已保存，重启 Agent 后生效');
    } catch (error) {
      setAgentModeFeedback(error instanceof Error ? error.message : '运行模式保存失败');
    } finally {
      setAgentModeBusy(false);
    }
  }

  async function chooseUnityEditor() {
    const result = await agentApi.pickUnityEditor();
    if (result.path) {
      setUnityEditorPath(result.path);
      setEditorFeedback('');
    }
  }

  async function saveUnityEditor(path = unityEditorPath) {
    setEditorSaving(true);
    setEditorFeedback('');
    try {
      const result = await agentApi.saveUnityEditor(path);
      onUnityEditorSettingsChange(result);
      setUnityEditorSettings(result);
      setUnityEditorPath(result.unity_editor_path);
      setEditorFeedback(path ? '已保存' : ['environment', 'discovered'].includes(result.source) ? '已恢复默认编辑器' : '已清除设置，当前没有可用编辑器');
    } catch {
      setEditorFeedback('无法保存，请确认 Unity.exe 路径后重试');
    } finally {
      setEditorSaving(false);
    }
  }

  async function detectRemoteClients() {
    if (remoteClientBusy) return;
    setRemoteClientBusy('detect');
    setRemoteClientFeedback({ sunlogin: '', todesk: '' });
    try {
      const overview = await agentApi.detectRemoteClients();
      onRemoteClientsChange(overview);
    } catch (error) {
      const message = error instanceof Error ? error.message : '自动检测失败，请手动选择客户端路径';
      setRemoteClientFeedback({ sunlogin: message, todesk: message });
    } finally {
      setRemoteClientBusy(null);
    }
  }

  async function chooseRemoteClient(vendor: RemoteClientVendor) {
    try {
      const result = await agentApi.pickRemoteClient(vendor);
      if (result.path) {
        setRemoteClientDrafts(current => ({ ...current, [vendor]: result.path || '' }));
        setRemoteClientFeedback(current => ({ ...current, [vendor]: '' }));
      }
    } catch (error) {
      setRemoteClientFeedback(current => ({ ...current, [vendor]: error instanceof Error ? error.message : '无法打开文件选择器' }));
    }
  }

  async function saveRemoteClient(vendor: RemoteClientVendor, path = remoteClientDrafts[vendor]) {
    if (remoteClientBusy) return;
    setRemoteClientBusy(vendor);
    setRemoteClientFeedback(current => ({ ...current, [vendor]: '' }));
    try {
      const overview = await agentApi.configureRemoteClient(vendor, path);
      onRemoteClientsChange(overview);
      const savedPath = overview.items.find(item => item.vendor === vendor)?.configured_path || '';
      setRemoteClientDrafts(current => ({ ...current, [vendor]: savedPath }));
      setRemoteClientFeedback(current => ({ ...current, [vendor]: '' }));
    } catch (error) {
      setRemoteClientFeedback(current => ({ ...current, [vendor]: error instanceof Error ? error.message : '保存失败，请确认路径指向客户端程序' }));
    } finally {
      setRemoteClientBusy(null);
    }
  }

  if (!settings || !remoteExecutionSettings || !loginState) return <div className="page-loading"><span className="spinner" />正在读取 Agent 配置</div>;
  const configured = loginState.status === 'credentials_configured';
  const editorState = unityEditorSettings || settings.editors;
  const editorDirty = unityEditorPath.trim() !== (editorState?.unity_editor_path || '');
  const editorStatus = editorState?.valid ? '可用' : editorState?.source === 'unset' ? '未配置' : '路径不可用';
  const editorSource = editorState?.source === 'agent' ? '自定义' : editorState?.source === 'environment' ? '团队默认' : editorState?.source === 'discovered' ? '本机安装' : '未设置';
  const updateRemoteExecution = (patch: Partial<RemoteExecutionSettings>) => {
    const next = { ...remoteExecutionSettings, ...patch };
    const enteringFullAccess = next.access_mode === 'full_access'
      && (remoteExecutionSettings.access_mode !== 'full_access' || (!remoteExecutionSettings.enabled && next.enabled));
    if (enteringFullAccess) setPendingFullAccess(next);
    else onRemoteExecutionChange(next);
  };
  const approvalProfile = (settings.profile === 'focus' ? 'balanced' : settings.profile || 'balanced') as ApprovalProfile;
  const profileOption = APPROVAL_PROFILE_OPTIONS.find(option => option.value === approvalProfile) || APPROVAL_PROFILE_OPTIONS[1];
  const exactRuleCount = exactApprovalRules.length;
  const effectiveModes = settings.effective_modes || fallbackEffectiveModes(approvalProfile, settings.rules || {});
  const changeApprovalProfile = (next: string) => {
    if (next === 'trusted') {
      setPendingApprovalTrust(true);
      return;
    }
    onApprovalProfileChange(next, next === 'trusted');
  };
  return (
    <>
      <PageHeader title="设置" description="管理任务权限、账号、工具与启动设置。" />
      <div className="settings-workspace">
        <label className="settings-section-select">
          <span>设置分类</span>
          <select value={section} onChange={event => setSection(event.target.value as SettingsSection)}>
            {SETTINGS_SECTIONS.map(item => <option key={item.key} value={item.key}>{item.label} · {item.description}</option>)}
          </select>
        </label>
        <nav className="settings-nav" aria-label="设置分类">
          {SETTINGS_SECTIONS.map(item => (
            <button key={item.key} className={section === item.key ? 'active' : ''} onClick={() => setSection(item.key)} aria-current={section === item.key ? 'page' : undefined}>
              <item.icon size={16} />
              <span><strong>{item.label}</strong><small>{item.description}</small></span>
            </button>
          ))}
        </nav>
        <div className="settings-content">
          {section === 'remote' ? <>
            <section className="card settings-section">
              <div className="card-header"><span>远程任务</span><Pill kind={remoteExecutionSettings.enabled ? 'success' : 'neutral'}>{remoteExecutionSettings.enabled ? '已启用' : '已关闭'}</Pill></div>
              <div className="card-body setting-list">
                <SettingRow title="接受远程任务" description="只接收当前工作台账号发给本机 Agent 的任务"><label className="toggle"><input type="checkbox" checked={remoteExecutionSettings.enabled} onChange={event => updateRemoteExecution({ enabled: event.target.checked })} /><span className="slider"></span></label></SettingRow>
                <SettingRow title="访问范围" description={remoteExecutionSettings.enabled ? '控制远程任务可操作的本机目录' : '启用远程任务后生效'}>
                  <select aria-label="远程任务访问范围" disabled={!remoteExecutionSettings.enabled} value={remoteExecutionSettings.access_mode} onChange={event => updateRemoteExecution({ access_mode: event.target.value as RemoteExecutionSettings['access_mode'] })}>
                    <option value="exhibit_linked">仅限展项关联目录（推荐）</option>
                    <option value="full_access">允许访问此电脑（高风险）</option>
                  </select>
                </SettingRow>
                <SettingRow title="执行工具" description={remoteExecutionSettings.enabled ? '自动模式会选择本机可用的 AI 工具' : '启用远程任务后生效'}>
                  <select aria-label="远程任务执行工具" disabled={!remoteExecutionSettings.enabled} value={remoteExecutionSettings.default_provider} onChange={event => updateRemoteExecution({ default_provider: event.target.value as RemoteExecutionSettings['default_provider'] })}>
                    <option value="himind.builtin">HiMind AI（推荐）</option><option value="auto">自动选择可用工具</option><option value="personal.codex">Codex</option><option value="personal.github-copilot">GitHub Copilot</option>
                  </select>
                </SettingRow>
              </div>
            </section>
            <section className="card settings-section builtin-ai-runtime-card">
              <div className="card-header">
                <span>HiMind AI</span>
                <Pill kind={runtimeReady ? 'success' : builtinAIRuntimeStatus ? 'warn' : 'neutral'}>
                  {runtimeWorking ? `${runtimeActionLabel(builtinAIRuntimeInstallation?.operation || 'install')}中` : builtinAIRuntimeInstallation?.update_available ? '有可用更新' : runtimeReady ? '已就绪' : builtinAIRuntimeStatus ? '需要安装' : '检测中'}
                </Pill>
              </div>
              <div className="runtime-summary">
                <div className="runtime-summary-main">
                  <div>
                    <strong>HiMind AI 运行时</strong>
                    <span>{builtinAIRuntimeInstallation?.message || builtinAIRuntimeStatus?.message || '正在检查运行时状态。'}</span>
                  </div>
                  <div className="actions-row runtime-actions">
                    <button className="btn btn-primary" disabled={builtinAIRuntimeBusy || builtinAIRuntimeCheckBusy || runtimeWorking} onClick={() => void primaryRuntimeAction()}>
                      {builtinAIRuntimeBusy || builtinAIRuntimeCheckBusy || runtimeWorking ? <LoaderCircle className="spin" size={15} /> : runtimeReady && builtinAIRuntimeInstallation?.update_available ? <Download size={15} /> : runtimeReady ? <RefreshCw size={15} /> : <Download size={15} />}
                      {runtimeWorking ? `${runtimeActionLabel(builtinAIRuntimeInstallation?.operation || 'install')}中 ${builtinAIRuntimeInstallation?.progress_percent || 0}%` : !runtimeReady ? '安装运行时' : builtinAIRuntimeInstallation?.update_available ? `更新到 v${builtinAIRuntimeInstallation.available_version}` : builtinAIRuntimeCheckBusy ? '检查中' : '检查更新'}
                    </button>
                    <details className="runtime-more-actions">
                      <summary title="更多操作" aria-label="更多操作"><MoreHorizontal size={17} /></summary>
                      <div>
                        {runtimeReady ? <button type="button" disabled={builtinAIRuntimeBusy || runtimeWorking} onClick={() => void startBuiltinAIRuntimeOperation('repair')}><Wrench size={14} />修复运行时</button> : null}
                        {runtimeReady ? <button type="button" className="danger-text" disabled={builtinAIRuntimeBusy || runtimeWorking} onClick={() => setPendingRuntimeUninstall(true)}><Trash2 size={14} />卸载运行时</button> : <button type="button" disabled={builtinAIRuntimeBusy || runtimeWorking} onClick={() => void refreshBuiltinAIRuntime()}><RefreshCw size={14} />重新检测</button>}
                      </div>
                    </details>
                  </div>
                </div>
                {runtimeWorking ? <div className="runtime-install-progress" role="status" aria-label={`${runtimeActionLabel(builtinAIRuntimeInstallation?.operation || 'install')}进度 ${builtinAIRuntimeInstallation?.progress_percent || 0}%`}><span style={{ width: `${builtinAIRuntimeInstallation?.progress_percent || 0}%` }} /></div> : null}
                {builtinAIRuntimeInstallation?.update_available ? <div className="runtime-update-notice"><strong>v{builtinAIRuntimeInstallation.available_version}</strong><span>{runtimeReleaseSummary(builtinAIRuntimeInstallation.release_notes)}</span></div> : null}
                {builtinAIRuntimeFeedback ? <div className="inline-feedback visible runtime-feedback" role="status">{builtinAIRuntimeFeedback}</div> : null}
                <details className="runtime-details">
                  <summary>开发者诊断</summary>
                  <div className="runtime-facts">
                    <div><span>契约版本</span><strong>v{builtinAIRuntimeStatus?.diagnostics.contract_version || 1}</strong></div>
                    <div><span>引擎</span><code>{builtinAIRuntimeStatus?.diagnostics.engine_id || '等待安装'}</code></div>
                    <div><span>引擎版本</span><strong>{builtinAIRuntimeStatus?.version || '未安装'}</strong></div>
                    <div><span>执行入口</span><code>{builtinAIRuntimeStatus?.diagnostics.executable_path || '等待安装'}</code></div>
                  </div>
                </details>
              </div>
            </section>
          </> : null}

          {section === 'approval' ? <section className="card settings-section approval-settings-card">
              <div className="card-header"><span>操作审批</span><Pill kind={approvalProfile === 'trusted' ? 'warn' : approvalProfile === 'silent_deny' ? 'neutral' : 'success'}>{profileOption.label}</Pill></div>
              <div className="approval-settings-body">
                <div className="approval-identity-row">
                  <div><strong>授权归属</strong><span>{independentMode ? '独立模式：策略保存在本机，仅约束经过 HiMind Agent 能力层的调用' : settings.owner_user_id ? '宽松授权与当前 Dashboard 账号和本机 Agent 绑定' : '当前审批设置仅保存在本机'}</span></div>
                  <Pill kind={independentMode || settings.owner_user_id ? 'success' : 'neutral'}>{independentMode ? '独立模式' : settings.owner_user_id ? `已绑定 ${settings.owner_user_id}` : '本机模式'}</Pill>
                </div>
                {independentMode ? <div className="security-note compact approval-independent-note"><ShieldCheck size={16} /><span>独立模式下 Dashboard Worker 不参与审批；本机审批队列、审批历史和右下角提醒仍可用。DSH 或外部 AI 工具直接执行且未经过 HiMind Agent 能力层的操作不受此策略拦截。</span></div> : null}

                <div className="approval-settings-group">
                  <div className="approval-group-heading"><strong>什么时候需要我确认</strong><span>单项例外会优先于整体设置</span></div>
                  <ChoiceGroup className="approval-profile-grid" label="操作审批确认程度" value={approvalProfile} options={APPROVAL_PROFILE_OPTIONS} onChange={changeApprovalProfile} />
                </div>

                <div className="approval-settings-group">
                  <div className="approval-group-heading"><strong>怎么提醒我</strong><span>不影响审批中心和托盘中的待处理数量</span></div>
                  <ChoiceGroup className="approval-notification-options" label="审批提醒方式" value={settings.notification_mode || 'popup'} options={APPROVAL_NOTIFICATION_OPTIONS} onChange={onApprovalNotificationModeChange} />
                </div>

                <div className="approval-settings-group approval-effective-group">
                  <div className="approval-group-heading"><strong>当前实际行为</strong><span>{exactRuleCount ? `另有 ${exactRuleCount} 条单项例外优先生效` : '没有额外的单项例外'}</span></div>
                  <div className="approval-effective-summary">
                    <ApprovalEffectiveItem icon={Search} label="查询与预览" mode={effectiveModes.read} />
                    <ApprovalEffectiveItem icon={PencilLine} label="新增与普通修改" mode={effectiveModes.write} />
                    <ApprovalEffectiveItem icon={Trash2} label="删除、发布与权限变更" mode={effectiveModes.high_risk} />
                    <ApprovalEffectiveItem icon={ShieldX} label="系统保护目标" mode="blocked" />
                  </div>
                </div>

                <details className="approval-advanced">
                  <summary><span><strong>高级设置与单项例外</strong><small>按操作类别或 Capability 精确调整</small></span><ChevronDown size={16} /></summary>
                  <div className="approval-advanced-body setting-list">
                    <SettingRow title="远程协助" description="从运维工作台主动发起的一键直连"><Pill kind="success">自动允许</Pill></SettingRow>
                    <SettingRow title="文件上传" description="代码、清单表和制品上传到本机"><ApprovalRuleChoice label="文件上传" value={approvalRuleValue(settings, 'upload_code')} onChange={mode => onRuleChange('upload_code', mode)} /></SettingRow>
                    <SettingRow title="查询与预览" description="只读取信息，不修改文件或业务数据"><ApprovalRuleChoice label="查询与预览" value={approvalRuleValue(settings, 'risk:R1')} onChange={mode => onRuleChange('risk:R1', mode)} /></SettingRow>
                    <SettingRow title="新增与普通修改" description="新增人员、更新展项或写入普通文件"><ApprovalRuleChoice label="新增与普通修改" value={approvalRuleValue(settings, 'risk:R2')} onChange={mode => onRuleChange('risk:R2', mode)} /></SettingRow>
                    <SettingRow title="删除、发布与权限变更" description="高风险操作；自动允许仅在完全信任下可用"><ApprovalRuleChoice label="删除、发布与权限变更" value={approvalRuleValue(settings, 'risk:R3')} autoApproveDisabled={approvalProfile !== 'trusted'} onChange={mode => onRuleChange('risk:R3', mode)} /></SettingRow>
                    <SettingRow title="其他受控操作" description="没有单独分类、但能力声明需要审批的操作"><ApprovalRuleChoice label="其他受控操作" value={approvalRuleValue(settings, 'controlled_operation')} onChange={mode => onRuleChange('controlled_operation', mode)} /></SettingRow>
                    <SettingRow title="未分类普通操作" description="兼容尚未声明操作类别的非高风险能力"><ApprovalRuleChoice label="未分类普通操作" value={approvalRuleValue(settings, '*')} onChange={mode => onRuleChange('*', mode)} /></SettingRow>
                    {exactApprovalRules.map(([requestType, mode]) => <SettingRow key={requestType} title={requestType} description="Capability 单项例外"><ApprovalRuleChoice label={requestType} value={mode as ApprovalRuleMode} autoApproveDisabled={requestType === 'risk:R4' || (isHighRiskRuleKey(requestType) && approvalProfile !== 'trusted')} onChange={next => onRuleChange(requestType, next)} /></SettingRow>)}
                    <SettingRow title="新增 Capability 例外" description="仅供插件或集成能力的精确授权">
                      <div className="approval-rule-editor">
                        <input aria-label="Capability ID" placeholder="例如 ai.client.import" value={approvalCapabilityId} onChange={event => setApprovalCapabilityId(event.target.value)} />
                        <ChoiceGroup className="approval-create-rule-options" label="新规则处理方式" value={approvalCapabilityMode} options={APPROVAL_CREATE_RULE_OPTIONS} onChange={value => setApprovalCapabilityMode(value as Exclude<ApprovalRuleMode, 'inherit'>)} />
                        <button type="button" className="btn" disabled={!approvalCapabilityId.trim()} onClick={() => { onRuleChange(approvalCapabilityId.trim(), approvalCapabilityMode); setApprovalCapabilityId(''); }}>保存</button>
                      </div>
                    </SettingRow>
                    <SettingRow title="等待确认时间" description="到期未处理时自动拒绝">
                      <ChoiceGroup className="approval-timeout-options" label="审批等待时间" value={String(settings.timeout_seconds)} options={[{ value: '15', label: '15 秒' }, { value: '30', label: '30 秒' }, { value: '60', label: '1 分钟' }, { value: '120', label: '2 分钟' }]} onChange={value => onTimeoutChange(Number(value))} />
                    </SettingRow>
                  </div>
                </details>
              </div>
            </section> : null}

          {section === 'remote-tools' ? <section className="card settings-section remote-client-settings">
              <div className="card-header"><span>远控工具</span><div className="card-header-actions"><button type="button" className="btn btn-icon" title="重新检测" aria-label="重新检测远控工具" disabled={remoteClientBusy !== null} onClick={() => void detectRemoteClients()}>{remoteClientBusy === 'detect' ? <LoaderCircle size={16} className="spin" /> : <RefreshCw size={16} />}</button></div></div>
            <div className="remote-client-body">
              <div className="remote-client-list">
                {REMOTE_CLIENT_OPTIONS.map(option => <RemoteClientCard key={option.vendor} option={option} status={remoteClients?.items.find(item => item.vendor === option.vendor)} path={remoteClientDrafts[option.vendor]} busy={remoteClientBusy === option.vendor} feedback={remoteClientFeedback[option.vendor]} onPathChange={path => { setRemoteClientDrafts(current => ({ ...current, [option.vendor]: path })); setRemoteClientFeedback(current => ({ ...current, [option.vendor]: '' })); }} onPick={() => void chooseRemoteClient(option.vendor)} onSave={() => void saveRemoteClient(option.vendor)} onClear={() => void saveRemoteClient(option.vendor, '')} />)}
              </div>
            </div>
          </section> : null}

          {section === 'accounts' ? <section className="card settings-section settings-credentials">
            <div className="card-header">账号</div>
            <div className="credential-section">
              <div className="credential-heading"><span>内网账号</span><Pill kind={configured ? 'success' : 'warn'}>{configured ? '已配置' : '待配置'}</Pill></div>
              <div className="account-row">
                <div className="account-icon"><KeyRound size={18} /></div>
                <div><span>当前账号</span><strong>{loginState.account || '未保存账号'}</strong></div>
                <button className="btn" onClick={onOpenLoginModal}>{configured ? '更新账号' : '配置账号'}</button>
              </div>
              <div className="security-note compact"><ShieldCheck size={16} /><span>账号信息加密保存在本机，不会写入工作台或日志。</span></div>
            </div>
            <div className="credential-section">
              <div className="credential-heading"><span>SVN 账号</span><Pill kind={svnConnections[0]?.status === 'ready' ? 'success' : 'warn'}>{!svnConnections.length ? '待配置' : svnConnections[0]?.status === 'ready' ? '已验证' : svnConnections[0]?.status === 'invalid' ? '凭据失效' : svnConnections[0]?.status === 'unreachable' ? '服务不可达' : '待验证'}</Pill></div>
              {svnConnections[0] ? <div className="svn-connection-row">
                <div className="account-icon"><Database size={17} /></div>
                <div className="svn-connection-main"><strong>{svnConnections[0].username}</strong><span>{svnConnections[0].base_url}</span><small>{svnConnections[0].status === 'invalid' ? '请更新账号并重新测试' : svnConnections[0].status === 'unreachable' ? '请检查 SVN 服务和本机客户端' : '项目仓库地址由项目自动生成'}</small></div>
                <div className="actions-row svn-connection-actions"><button className="btn" disabled={svnTesting} onClick={onTestSvnConnection}>{svnTesting ? <><LoaderCircle size={14} className="spin" />测试中</> : '测试'}</button><button className="btn" disabled={svnTesting} onClick={onOpenSvnModal}>更新账号</button><button className="btn btn-danger-quiet" disabled={svnTesting} onClick={onRemoveSvnConnection}>清除</button></div>
              </div> : <div className="account-row"><div className="account-icon"><Database size={17} /></div><div><span>公司 SVN</span><strong>尚未配置个人账号</strong></div><button className="btn btn-primary" onClick={onOpenSvnModal}>配置账号</button></div>}
              <div className="security-note compact"><ShieldCheck size={16} /><span>密码只保存在当前 Windows 用户的本地加密存储中。</span></div>
            </div>
          </section> : null}

          {section === 'tools' ? <section className="card settings-section unity-editor-card">
            <div className="card-header"><span>Unity 编辑器</span><Pill kind={editorState?.valid ? 'success' : 'warn'}>{editorStatus}</Pill></div>
            <div className="unity-editor-body">
              <div className="unity-editor-source">
                <div><span>当前来源</span><strong>{editorSource}</strong></div>
                <small>{editorState?.source === 'environment' ? '当前使用团队提供的默认编辑器。' : editorState?.source === 'discovered' ? '已从本机常规安装目录发现 Unity 编辑器。' : '没有团队默认编辑器时，可以在此选择 Unity.exe。'}</small>
              </div>
              <label className="unity-editor-field" htmlFor="unity-editor-path">
                <span>Unity.exe 路径</span>
                <div className="unity-editor-input">
                  <input id="unity-editor-path" value={unityEditorPath} onChange={event => { setUnityEditorPath(event.target.value); setEditorFeedback(''); }} placeholder="请选择 Unity.exe" />
                  <button className="btn btn-icon" title="选择 Unity.exe" aria-label="选择 Unity.exe" onClick={chooseUnityEditor}><FolderOpen size={16} /></button>
                </div>
              </label>
              <div className="unity-editor-footer">
                <span className={editorFeedback ? 'inline-feedback visible' : 'inline-feedback'}>{editorFeedback || (editorDirty ? '路径尚未保存' : '')}</span>
                <div className="unity-editor-actions">
                  <button className="btn" disabled={editorSaving || (!editorDirty && editorState?.source !== 'agent')} onClick={() => saveUnityEditor('')}><RotateCcw size={15} />恢复默认</button>
                  <button className="btn btn-primary" disabled={editorSaving || !editorDirty || !unityEditorPath.trim()} onClick={() => saveUnityEditor()}>{editorSaving ? <LoaderCircle className="spin" size={15} /> : <Save size={15} />}保存</button>
                </div>
              </div>
            </div>
          </section> : null}

          {section === 'general' ? <>
            <section className="card settings-section">
              <div className="card-header"><span>运行模式</span><Pill kind={agentMode?.mode === 'independent' ? 'success' : 'neutral'}>{agentModeLabel(agentMode?.mode)}</Pill></div>
              <div className="card-body setting-list">
                <SettingRow title="Agent 运行模式" description="独立模式适合个人使用；组织模式适合接入 HiMind 工作台。">
                  <div className="mode-options" role="radiogroup" aria-label="Agent 运行模式">
                    <label className={agentMode?.mode === 'connected' ? 'mode-option active' : 'mode-option'}><input type="radio" name="agent-mode" checked={agentMode?.mode === 'connected'} disabled={!agentMode || agentModeBusy} onChange={() => void changeAgentMode('connected')} /><span>组织模式</span></label>
                    <label className={agentMode?.mode === 'independent' ? 'mode-option active' : 'mode-option'}><input type="radio" name="agent-mode" checked={agentMode?.mode === 'independent'} disabled={!agentMode || agentModeBusy} onChange={() => void changeAgentMode('independent')} /><span>独立模式</span></label>
                  </div>
                </SettingRow>
                {agentMode ? <div className="mode-state-summary" role="status">
                  <span>当前生效：{agentModeLabel(agentMode.effective_mode)}</span>
                  <span>重启后：{agentModeLabel(agentMode.pending_mode)}</span>
                  {agentMode.requires_restart ? <strong>重启 Agent 后切换</strong> : null}
                </div> : null}
                {agentModeFeedback ? <div className="inline-feedback visible" role="status">{agentModeFeedback}</div> : null}
              </div>
            </section>
            <section className="card settings-section">
              <div className="card-header">软件更新</div>
              <div className="software-update-summary">
                <div><span>当前版本</span><strong>v{updateStatus?.current_version || '—'}</strong></div>
                <div><span>最近检查</span><strong>{formatUpdateTime(updateStatus?.last_checked_at)}</strong></div>
              </div>
              <div className="card-body setting-list">
                <SettingRow title="自动检查更新" description="定期检查是否有新版本"><label className="toggle"><input type="checkbox" checked={updateStatus?.auto_check ?? true} onChange={event => onUpdatePreferences(event.target.checked, event.target.checked && (updateStatus?.auto_download ?? true))} /><span className="slider"></span></label></SettingRow>
                <SettingRow title="自动下载更新" description="有新版本时在后台下载，安装前会通知你"><label className="toggle"><input type="checkbox" disabled={!updateStatus?.auto_check} checked={updateStatus?.auto_download ?? true} onChange={event => onUpdatePreferences(updateStatus?.auto_check ?? true, event.target.checked)} /><span className="slider"></span></label></SettingRow>
              </div>
              <div className="software-update-state">
                <div>
                  <strong>{describeUpdateState(updateStatus)}</strong>
                  <span>{describeUpdateMessage(updateStatus)}</span>
                  {updateStatus?.status === 'downloading' ? <div className="agent-update-progress"><span style={{ width: `${updateStatus.progress_percent}%` }} /></div> : null}
                </div>
                <div className="actions-row">
                  {updateStatus?.status === 'downloading' ? <button className="btn" onClick={onCancelUpdateDownload}>取消下载</button> : null}
                  {updateStatus?.available_version && !['downloading', 'ready', 'installing'].includes(updateStatus.status) ? <button className="btn" disabled={updateBusy} onClick={onDownloadUpdate}><Download size={15} />下载更新</button> : null}
                  {updateStatus?.status === 'ready' ? <button className="btn btn-primary" disabled={updateBusy} onClick={onInstallUpdate}><RefreshCw size={15} />重启并更新</button> : null}
                  <button className="btn" disabled={updateBusy || ['checking', 'downloading', 'installing'].includes(updateStatus?.status || '')} onClick={onCheckUpdate}>{updateBusy || updateStatus?.status === 'checking' ? <LoaderCircle className="spin" size={15} /> : <RefreshCw size={15} />}检查更新</button>
                </div>
              </div>
            </section>
            <section className="card settings-section">
              <div className="card-header">Agent 启动</div>
              <div className="card-body setting-list">
                <SettingRow title="开机自启" description="登录 Windows 后自动启动 HiMind Agent"><label className="toggle"><input type="checkbox" checked={settings.auto_start} onChange={event => onAutoStartChange(event.target.checked)} /><span className="slider"></span></label></SettingRow>
              </div>
            </section>
          </> : null}
        </div>
      </div>
      {loginModalOpen ? <LoginModal configured={configured} username={loginUsername} password={loginPassword} onClose={onCloseLoginModal} onUsernameChange={onUsernameChange} onPasswordChange={onPasswordChange} onSave={onSaveLogin} onLogout={onLogoutLogin} onOpenInnerAdmin={onOpenInnerAdmin} /> : null}
      {svnModalOpen ? <SvnConnectionModal draft={svnDraft} exists={svnConnections.length > 0} onClose={onCloseSvnModal} onChange={onSvnDraftChange} onSave={onSaveSvnConnection} /> : null}
      {pendingApprovalTrust ? <ApprovalTrustConfirmation onClose={() => setPendingApprovalTrust(false)} onConfirm={() => { onApprovalProfileChange('trusted', true); setPendingApprovalTrust(false); }} /> : null}
      {pendingFullAccess ? <FullAccessConfirmation onClose={() => setPendingFullAccess(null)} onConfirm={() => { onRemoteExecutionChange(pendingFullAccess, true); setPendingFullAccess(null); }} /> : null}
      {pendingRuntimeUninstall ? <RuntimeUninstallConfirmation onClose={() => setPendingRuntimeUninstall(false)} onConfirm={() => { setPendingRuntimeUninstall(false); void startBuiltinAIRuntimeOperation('uninstall'); }} /> : null}
    </>
  );
}

function runtimeActionLabel(operation: string) {
  if (operation === 'update') return '更新';
  if (operation === 'repair') return '修复';
  if (operation === 'uninstall') return '卸载';
  return '安装';
}

function runtimeReleaseSummary(releaseNotes: string) {
  const summary = releaseNotes.replace(/\s+/g, ' ').trim() || '包含稳定性和兼容性改进。';
  return summary.length > 180 ? `${summary.slice(0, 180)}...` : summary;
}

function formatUpdateTime(timestamp?: number) {
  if (!timestamp) return '尚未检查';
  return new Date(timestamp * 1000).toLocaleString('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' });
}

function describeUpdateState(status: AgentUpdateStatus | null) {
  if (!status) return '正在读取更新状态';
  if (status.status === 'checking') return '正在检查更新';
  if (status.status === 'downloading') return `正在下载 v${status.available_version} · ${status.progress_percent}%`;
  if (status.status === 'ready') return `v${status.available_version} 更新已下载`;
  if (status.status === 'installing') return '正在重启并安装更新';
  if (status.status === 'failed') return '更新未完成';
  if (status.status === 'rolled_back') return '新版本未能启动，已恢复上一版本';
  if (status.available_version) return `可更新到 v${status.available_version}`;
  return '当前已是最新版本';
}

function describeUpdateMessage(status: AgentUpdateStatus | null) {
  if (!status) return '正在读取更新信息。';
  if (status.status === 'checking') return '请稍候。';
  if (status.status === 'downloading') return '下载完成后会通知你。';
  if (status.status === 'ready') return '重启 HiMind Agent 后完成安装。';
  if (status.status === 'installing') return '更新完成后会自动重新启动。';
  if (status.status === 'failed') return '暂时无法完成更新，请稍后重试。';
  if (status.status === 'rolled_back') return '更新没有完成，仍在使用上一版本。';
  return status.release_notes || 'HiMind Agent 会定期检查更新。';
}

function RuntimeUninstallConfirmation({ onClose, onConfirm }: { onClose: () => void; onConfirm: () => void }) {
  return (
    <div className="modal-backdrop" role="presentation">
      <div className="modal" role="dialog" aria-modal="true" aria-labelledby="runtime-uninstall-title">
        <div className="modal-header"><div><h3 id="runtime-uninstall-title">卸载 HiMind AI 运行时？</h3><p>HiMind AI 将暂时不可用，个人 AI 服务、技能、插件和用户数据会保留。</p></div><IconButton icon={X} label="关闭" onClick={onClose} /></div>
        <div className="modal-body"><div className="modal-actions"><span /><div className="actions-row"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-danger" onClick={onConfirm}><Trash2 size={15} />确认卸载</button></div></div></div>
      </div>
    </div>
  );
}

function FullAccessConfirmation({ onClose, onConfirm }: { onClose: () => void; onConfirm: () => void }) {
  return (
    <div className="modal-backdrop" onClick={onClose} role="presentation">
      <div className="modal" role="dialog" aria-modal="true" aria-labelledby="full-access-title" onClick={event => event.stopPropagation()}>
        <div className="modal-header"><div><h3 id="full-access-title">允许访问此电脑？</h3><p>仅在远程任务确实需要时开启。</p></div><IconButton icon={X} label="关闭" onClick={onClose} /></div>
        <div className="modal-body">
          <div className="full-access-warning"><ShieldAlert size={20} /><div><strong>远程任务可以访问你有权限使用的文件和文件夹</strong><span>任务可能读取、创建或修改展项目录以外的内容。只对可信任务开启此权限。</span></div></div>
          <div className="modal-actions"><span /><div className="actions-row"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-danger" onClick={onConfirm}><Bot size={15} />确认启用</button></div></div>
        </div>
      </div>
    </div>
  );
}

function ApprovalTrustConfirmation({ onClose, onConfirm }: { onClose: () => void; onConfirm: () => void }) {
  return (
    <div className="modal-backdrop" onClick={onClose} role="presentation">
      <div className="modal approval-trust-modal" role="dialog" aria-modal="true" aria-labelledby="approval-trust-title" onClick={event => event.stopPropagation()}>
        <div className="modal-header approval-trust-header">
          <div className="approval-trust-heading"><div className="approval-trust-icon"><ShieldAlert size={20} /></div><div><h3 id="approval-trust-title">启用完全信任</h3><p>未来 1 小时内，R3 高风险操作可以自动执行。</p></div></div>
          <IconButton icon={X} label="关闭" onClick={onClose} />
        </div>
        <div className="modal-body approval-trust-body">
          <div className="approval-trust-intro"><strong>你将把审批交给当前 Agent 自主判断</strong><span>风险由当前用户自行承担；授权仅作用于当前 Agent，可随时在审批中心或设置中恢复严格审批。</span></div>
          <div className="approval-trust-scope">
            <div className="approval-trust-scope-item"><FolderOpen size={17} /><div><strong>本地文件</strong><span>删除、批量清理或覆盖工作区文件。</span></div></div>
            <div className="approval-trust-scope-item"><Database size={17} /><div><strong>Dashboard 业务数据</strong><span>删除项目/展项、解除关联、替换人员、发布变更。</span></div></div>
            <div className="approval-trust-scope-item"><Globe2 size={17} /><div><strong>第三方与 MCP</strong><span>符合 R3 契约的外部写操作和集成调用。</span></div></div>
          </div>
          <div className="approval-trust-boundaries"><LockKeyhole size={16} /><div><strong>仍然不会放行</strong><span>R4 操作、系统保护目录、Agent 数据目录、Dashboard ACL 和服务端硬拒绝。</span></div></div>
          <div className="approval-trust-meta"><span><Clock3 size={14} />有效期 1 小时</span><span><CheckCircle2 size={14} />可随时撤销</span></div>
          <div className="modal-actions"><span /><div className="actions-row"><button className="btn" onClick={onClose}>暂不启用</button><button className="btn btn-danger" onClick={onConfirm}><KeyRound size={15} />确认授权 1 小时</button></div></div>
        </div>
      </div>
    </div>
  );
}

function SvnConnectionModal({ draft, exists, onClose, onChange, onSave }: { draft: SvnConnectionInput; exists: boolean; onClose: () => void; onChange: (draft: SvnConnectionInput) => void; onSave: () => void }) {
  const update = (field: keyof SvnConnectionInput, value: string) => onChange({ ...draft, [field]: value });
  return (
    <div className="modal-backdrop" onClick={onClose} role="presentation">
      <div className="modal" role="dialog" aria-modal="true" aria-labelledby="svn-modal-title" onClick={event => event.stopPropagation()}>
        <div className="modal-header"><div><h3 id="svn-modal-title">{exists ? '更新 SVN 账号' : '配置 SVN 账号'}</h3><p>用于访问公司 SVN 中当前账号有权限的项目仓库。</p></div><IconButton icon={X} label="关闭" onClick={onClose} /></div>
        <div className="modal-body">
          <div className="field-group"><label className="field-label" htmlFor="svn-username">账号</label><input id="svn-username" autoComplete="username" value={draft.username} onChange={event => update('username', event.target.value)} /></div>
          <div className="field-group"><label className="field-label" htmlFor="svn-password">密码</label><input id="svn-password" autoComplete="current-password" type="password" value={draft.password} onChange={event => update('password', event.target.value)} placeholder={exists ? '留空以保留当前密码' : '输入 SVN 密码'} /></div>
          <div className="modal-actions"><span /><div className="actions-row"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-primary" onClick={onSave} disabled={!draft.username.trim() || (!exists && !draft.password)}>保存账号</button></div></div>
        </div>
      </div>
    </div>
  );
}

function SettingRow({ title, description, children }: { title: string; description: string; children: ReactNode }) {
  return (
    <div className="setting-row">
      <div><div className="label-text">{title}</div><div className="label-desc">{description}</div></div>
      <div className="setting-control">{children}</div>
    </div>
  );
}

type ChoiceOption = {
  value: string;
  label: string;
  description?: string;
  icon?: typeof ShieldCheck;
  disabled?: boolean;
};

function ChoiceGroup({ className = '', label, value, options, onChange }: {
  className?: string;
  label: string;
  value: string;
  options: readonly ChoiceOption[];
  onChange: (value: string) => void;
}) {
  return (
    <div className={`approval-choice-group ${className}`} role="radiogroup" aria-label={label}>
      {options.map(option => {
        const Icon = option.icon;
        const active = option.value === value;
        return (
          <button key={option.value} type="button" role="radio" aria-checked={active} className={active ? 'active' : ''} disabled={option.disabled} onClick={() => onChange(option.value)}>
            {Icon ? <Icon size={16} /> : null}
            <span><strong>{option.label}</strong>{option.description ? <small>{option.description}</small> : null}</span>
          </button>
        );
      })}
    </div>
  );
}

function ApprovalRuleChoice({ label, value, autoApproveDisabled = false, onChange }: {
  label: string;
  value: ApprovalRuleMode;
  autoApproveDisabled?: boolean;
  onChange: (mode: ApprovalRuleMode) => void;
}) {
  const options = APPROVAL_RULE_OPTIONS.map(option => option.value === 'auto_approve' && autoApproveDisabled ? { ...option, disabled: true } : option);
  return <ChoiceGroup className="approval-rule-choice" label={`${label}处理方式`} value={value} options={options} onChange={mode => onChange(mode as ApprovalRuleMode)} />;
}

function ApprovalEffectiveItem({ icon: Icon, label, mode }: {
  icon: typeof ShieldCheck;
  label: string;
  mode: 'manual' | 'auto_approve' | 'auto_deny' | 'blocked';
}) {
  const modeLabel = mode === 'auto_approve' ? '自动执行' : mode === 'manual' ? '需要确认' : mode === 'auto_deny' ? '自动拒绝' : '始终阻止';
  return <div className={`approval-effective-item ${mode}`}><Icon size={16} /><span>{label}</span><strong>{modeLabel}</strong></div>;
}

function approvalRuleValue(settings: ApprovalSettings, key: string): ApprovalRuleMode {
  return (settings.rules?.[key] || 'inherit') as ApprovalRuleMode;
}

function fallbackEffectiveModes(profile: ApprovalProfile, rules: Record<string, string>) {
  const resolve = (risk: 'R1' | 'R2' | 'R3'): 'manual' | 'auto_approve' | 'auto_deny' => {
    const rank = Number(risk.slice(1));
    for (const key of [`risk:${risk}`, 'controlled_operation', '*']) {
      const mode = rules[key];
      if (mode === 'auto_deny') return 'auto_deny';
      if (mode === 'manual') return 'manual';
      if (mode === 'auto_approve' && (rank < 3 || profile === 'trusted')) return 'auto_approve';
    }
    if (profile === 'silent_deny') return 'auto_deny';
    if (profile === 'trusted' && rank <= 3) return 'auto_approve';
    if (profile === 'relaxed' && rank <= 2) return 'auto_approve';
    if (profile === 'balanced' && rank <= 1) return 'auto_approve';
    return 'manual';
  };
  return { read: resolve('R1'), write: resolve('R2'), high_risk: resolve('R3') };
}

function isHighRiskRuleKey(key: string) {
  const normalized = key.trim().toLowerCase();
  return normalized === 'risk:r3'
    || normalized.endsWith('.delete')
    || normalized.endsWith('.delete_all')
    || normalized === 'business.project.managers.replace'
    || normalized === 'business.project.owners.replace'
    || normalized === 'business.exhibit.crew.replace'
    || normalized === 'business.project.exhibit.detach'
    || normalized === 'software.distribution.release.publish'
    || normalized === 'extension.review.decide';
}

const REMOTE_CLIENT_OPTIONS = [
  { vendor: 'todesk', name: 'ToDesk', description: 'ToDesk 远程协助客户端' },
  { vendor: 'sunlogin', name: '向日葵', description: '向日葵远程控制客户端' },
] satisfies { vendor: RemoteClientVendor; name: string; description: string }[];

function remoteClientDraftsFromOverview(overview: RemoteClientOverview): Record<RemoteClientVendor, string> {
  return overview.items.reduce((drafts, item) => ({ ...drafts, [item.vendor]: item.configured_path || '' }), { sunlogin: '', todesk: '' } as Record<RemoteClientVendor, string>);
}

function RemoteClientCard({ option, status, path, busy, feedback, onPathChange, onPick, onSave, onClear }: {
  option: { vendor: RemoteClientVendor; name: string; description: string };
  status?: RemoteClientStatus;
  path: string;
  busy: boolean;
  feedback: string;
  onPathChange: (path: string) => void;
  onPick: () => void;
  onSave: () => void;
  onClear: () => void;
}) {
  const persistedPath = status?.configured_path || '';
  const detectedPath = status?.resolved_path || '';
  const configuredInvalid = Boolean(persistedPath && status?.configured_valid === false);
  const dirty = path.trim() !== persistedPath;
  return (
    <div className={`remote-client-row${configuredInvalid ? ' invalid' : ''}`}>
      <div className="remote-client-heading">
        <div className="remote-client-icon"><Monitor size={16} /></div>
        <strong>{option.name}</strong>
      </div>
      <div className="remote-client-path-input">
        <input id={`remote-client-${option.vendor}`} aria-label={`${option.name}路径`} value={path} onChange={event => onPathChange(event.target.value)} placeholder={detectedPath || `选择 ${option.name}.exe`} title={path || detectedPath || ''} />
        <button type="button" className="btn btn-icon" title={`选择 ${option.name} 程序`} aria-label={`选择 ${option.name} 程序`} disabled={busy} onClick={onPick}><FolderOpen size={16} /></button>
      </div>
      <div className="remote-client-footer">
        {feedback ? <span className="inline-feedback visible" role="status">{feedback}</span> : null}
        <div className="remote-client-actions">
          {status?.configured_by === 'manual' ? <button type="button" className="btn btn-danger-quiet" title="清除手动路径" aria-label={`清除 ${option.name} 手动路径`} disabled={busy} onClick={onClear}><Trash2 size={14} /></button> : null}
          <button type="button" className="btn btn-icon btn-primary" title="保存路径" aria-label={`保存 ${option.name} 路径`} disabled={busy || !dirty || (!path.trim() && !persistedPath)} onClick={onSave}>{busy ? <LoaderCircle size={14} className="spin" /> : <Save size={14} />}</button>
        </div>
      </div>
    </div>
  );
}

function LoginModal({ configured, username, password, onClose, onUsernameChange, onPasswordChange, onSave, onLogout, onOpenInnerAdmin }: { configured: boolean; username: string; password: string; onClose: () => void; onUsernameChange: (value: string) => void; onPasswordChange: (value: string) => void; onSave: () => void; onLogout: () => void; onOpenInnerAdmin: () => void }) {
  return (
    <div className="modal-backdrop" onClick={onClose} role="presentation">
      <div className="modal" role="dialog" aria-modal="true" aria-labelledby="login-modal-title" onClick={event => event.stopPropagation()}>
        <div className="modal-header"><div><h3 id="login-modal-title">配置内网账号</h3><p>凭据仅保存在当前 Windows Agent。</p></div><IconButton icon={X} label="关闭" onClick={onClose} /></div>
        <div className="modal-body">
          <div className="field-group"><label className="field-label" htmlFor="login-username">内网账号</label><input id="login-username" autoComplete="username" value={username} onChange={event => onUsernameChange(event.target.value)} placeholder="输入内网平台用户名" /></div>
          <div className="field-group"><label className="field-label" htmlFor="login-password">内网密码</label><input id="login-password" autoComplete="current-password" type="password" value={password} onChange={event => onPasswordChange(event.target.value)} placeholder={configured ? '输入新密码以更新凭据' : '输入内网平台密码'} /></div>
          <button className="text-action" onClick={onOpenInnerAdmin}><ExternalLink size={14} />打开内网平台</button>
          <div className="modal-actions"><div>{configured ? <button className="btn btn-danger-quiet" onClick={onLogout}>清除凭据</button> : null}</div><div className="actions-row"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-primary" onClick={onSave} disabled={!username.trim() || !password}>保存凭据</button></div></div>
        </div>
      </div>
    </div>
  );
}
