import { useEffect, useRef, useState, type ReactNode } from 'react';
import { agentApi, type AgentBackupExportReport, type AgentBackupInspectReport, type AgentBackupRestoreReport, type AgentBackupScope, type AgentUpdateStatus, type ApprovalSettings, type BuiltinAIRuntimeInstallationStatus, type BuiltinAIRuntimeStatus, type ConnectorStateItem, type DashboardAuthorizationProgress, type DashboardIdentityStatus, type EngineInstallation, type GithubAccountStatus, type GithubAppAuthorization, type GithubAppInstallation, type LogItem, type LoginState, type RemoteClientOverview, type RemoteClientStatus, type RemoteClientVendor, type RemoteExecutionSettings, type SvnConnection, type SvnConnectionInput, type UnityEditorSettings, type WorkbenchConnection, type WorkbenchConnectionsSnapshot, type WorkbenchProbe } from '../services/agentApi';
import { Bell, BellOff, Bot, Check, CheckCircle2, ChevronDown, Clock3, Database, Download, ExternalLink, FolderOpen, Github, Globe2, Inbox, KeyRound, Link2, LockKeyhole, LogIn, Monitor, MoreHorizontal, PencilLine, PlugZap, Power, RefreshCw, RotateCcw, ScanSearch, Search, ShieldAlert, ShieldCheck, ShieldX, Trash2, UnlockKeyhole, Wrench, X } from 'lucide-react';
import { BusyIndicator } from '../components/BusyIndicator';
import { useConfirm } from '../components/ConfirmDialog';
import { IconButton, PageHeader, Pill } from '../components/Common';
import { ActionMenu, ActionMenuItem } from '../components/ActionMenu';
import { WorkbenchConnectionsPanel, type WorkbenchConnectionDraft } from '../components/WorkbenchConnectionsPanel';
import { LogsPage } from './LogsPage';
import { localRuntimeMeta } from './runtimeProviderView';
import { settingsSectionMeta, settingsSectionTabFor, settingsSectionTabs, type SettingsSection, type SettingsTab } from '../settingsModel';

const BUILTIN_APPROVAL_RULES = new Set(['remote_connect', 'upload_code', 'upload_placeholder', 'controlled_operation', '*', 'risk:R1', 'risk:R2', 'risk:R3', 'risk:R4']);

/** Tauri 拒绝时抛出的通常是字符串，这里统一取出可读原因，避免把真实失败原因盖成通用文案。 */
function failureText(error: unknown, fallback: string) {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === 'string' && error.trim()) return error.trim();
  return fallback;
}

type ApprovalProfile = 'strict' | 'balanced' | 'relaxed' | 'trusted' | 'full_access' | 'silent_deny';
type ApprovalRuleMode = 'inherit' | 'manual' | 'auto_approve' | 'auto_deny';

function connectorAvailabilityLabel(availability: string) {
  if (availability === 'local') return '本机连接';
  if (availability === 'network_service') return '网络服务';
  if (availability === 'control_plane') return '工作台服务';
  return '自定义连接';
}

const APPROVAL_PROFILE_OPTIONS = [
  { value: 'strict', label: '更安全', description: '除单独授权外，受控操作都先确认', icon: ShieldAlert },
  { value: 'balanced', label: '推荐', description: '查询预览自动执行，修改操作先确认', icon: ShieldCheck },
  { value: 'relaxed', label: '少打扰', description: '查询和普通修改自动执行', icon: BellOff },
  { value: 'trusted', label: '完全信任', description: '常规及高风险操作自动执行，最高风险仍确认', icon: KeyRound },
  { value: 'full_access', label: '完全放行', description: '所有受控操作自动执行，含工作流步骤与最高风险操作', icon: UnlockKeyhole },
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
  identity,
  onRevokeAuthorization,
  workbenchConnections,
  workbenchConnectionsError,
  workbenchBusyId = '',
  workbenchAuthorization,
  onRefreshWorkbenchConnections,
  onAddWorkbenchConnection,
  onRenameWorkbenchConnection,
  onRemoveWorkbenchConnection,
  onSwitchWorkbenchConnection,
  onEnrollWorkbenchConnection,
  onProbeWorkbenchConnection,
  onAuthorizeWorkbenchConnection,
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
  logs,
  onExportDiagnostics,
  independentMode = false,
  section,
  tab = null,
  onTabChange,
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
  identity: DashboardIdentityStatus | null;
  onRevokeAuthorization: () => void;
  workbenchConnections: WorkbenchConnectionsSnapshot | null;
  /** 连接清单读取失败的原因；有值时面板显示错误态而不是「读取中」。 */
  workbenchConnectionsError?: string;
  workbenchBusyId?: string;
  workbenchAuthorization: DashboardAuthorizationProgress | null;
  onRefreshWorkbenchConnections: () => void;
  onAddWorkbenchConnection: (draft: WorkbenchConnectionDraft) => Promise<void>;
  onRenameWorkbenchConnection: (id: string, displayName: string, purpose: string) => Promise<void>;
  onRemoveWorkbenchConnection: (connection: WorkbenchConnection) => void;
  onSwitchWorkbenchConnection: (connection: WorkbenchConnection) => void;
  onEnrollWorkbenchConnection: (id: string, enrollmentToken: string) => Promise<void>;
  onProbeWorkbenchConnection: (apiBase: string) => Promise<WorkbenchProbe>;
  onAuthorizeWorkbenchConnection: (connection: WorkbenchConnection) => void;
  onRemoteExecutionChange: (settings: RemoteExecutionSettings, fullAccessConfirmed?: boolean) => void;
  onRemoteClientsChange: (overview: RemoteClientOverview) => void;
  onRuleChange: (requestType: string, mode: string) => void;
  onApprovalProfileChange: (profile: string, confirmed?: boolean, durationSeconds?: number) => void;
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
  logs: LogItem[];
  onExportDiagnostics: () => void;
  independentMode?: boolean;
  section: SettingsSection;
  /** 页内页签：条目把无先后关系的板块并在一处，默认落到第一块。 */
  tab?: SettingsTab | null;
  onTabChange?: (tab: SettingsTab) => void;
}) {
  const confirm = useConfirm();
  const [builtinAIRuntimeStatus, setBuiltinAIRuntimeStatus] = useState<BuiltinAIRuntimeStatus | null>(null);
  const [builtinAIRuntimeInstallation, setBuiltinAIRuntimeInstallation] = useState<BuiltinAIRuntimeInstallationStatus | null>(null);
  const [builtinAIRuntimeBusy, setBuiltinAIRuntimeBusy] = useState(false);
  const [builtinAIRuntimeCheckBusy, setBuiltinAIRuntimeCheckBusy] = useState(false);
  const [builtinAIRuntimeFeedback, setBuiltinAIRuntimeFeedback] = useState('');
  const [connectorStates, setConnectorStates] = useState<ConnectorStateItem[]>([]);
  const [connectorBusy, setConnectorBusy] = useState('');
  const [githubAccount, setGithubAccount] = useState<GithubAccountStatus | null>(null);
  const [githubToken, setGithubToken] = useState('');
  const [githubBusy, setGithubBusy] = useState(false);
  const [githubFeedback, setGithubFeedback] = useState('');
  const [githubMethod, setGithubMethod] = useState<'app' | 'pat'>('app');
  const [githubClientId, setGithubClientId] = useState('');
  const [githubDevice, setGithubDevice] = useState<(GithubAppAuthorization & { client_id: string }) | null>(null);
  const [githubInstallations, setGithubInstallations] = useState<GithubAppInstallation[]>([]);
  const [connectorFeedback, setConnectorFeedback] = useState('');
  const [pendingRuntimeUninstall, setPendingRuntimeUninstall] = useState(false);
  const [backupScope, setBackupScope] = useState<AgentBackupScope | null>(null);
  const [backupPassphrase, setBackupPassphrase] = useState('');
  const [backupIncludeDeviceIdentity, setBackupIncludeDeviceIdentity] = useState(false);
  const [backupBusy, setBackupBusy] = useState<'' | 'export' | 'inspect' | 'restore'>('');
  const [backupFeedback, setBackupFeedback] = useState('');
  const [backupExportReport, setBackupExportReport] = useState<AgentBackupExportReport | null>(null);
  const [backupInspectReport, setBackupInspectReport] = useState<AgentBackupInspectReport | null>(null);
  const [backupRestoreReport, setBackupRestoreReport] = useState<AgentBackupRestoreReport | null>(null);
  const [pendingBackupRestore, setPendingBackupRestore] = useState(false);
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
        if (next.state === 'ready' || next.state === 'idle') setBuiltinAIRuntimeFeedback(presentRuntimeMessage(next.message));
        if (next.state === 'failed') setBuiltinAIRuntimeFeedback(next.error || 'HiMind AI 安装失败，请重试');
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
      setBuiltinAIRuntimeFeedback('暂时无法检查 HiMind AI，请稍后重试');
    }
  };
  const checkBuiltinAIRuntimeUpdate = async () => {
    if (builtinAIRuntimeCheckBusy || runtimeWorking) return;
    setBuiltinAIRuntimeCheckBusy(true);
      setBuiltinAIRuntimeFeedback('正在检查 HiMind AI 更新');
    try {
      const installation = await agentApi.checkBuiltinAiRuntimeUpdate();
      setBuiltinAIRuntimeInstallation(installation);
      setBuiltinAIRuntimeStatus(installation.runtime);
      setBuiltinAIRuntimeFeedback(presentRuntimeMessage(installation.message));
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error || '');
      setBuiltinAIRuntimeFeedback(message.includes('尚未安装') ? '请先安装 HiMind AI' : '暂时无法检查更新，请稍后重试');
    } finally {
      setBuiltinAIRuntimeCheckBusy(false);
    }
  };
  const startBuiltinAIRuntimeOperation = async (operation: BuiltinAIRuntimeInstallationStatus['operation'], manifestPath?: string) => {
    if (builtinAIRuntimeBusy || runtimeWorking) return;
    setBuiltinAIRuntimeBusy(true);
    setBuiltinAIRuntimeFeedback('');
    try {
      const installation = await agentApi.startBuiltinAiRuntimeInstall(operation, manifestPath);
      setBuiltinAIRuntimeInstallation(installation);
      setBuiltinAIRuntimeStatus(installation.runtime);
      setBuiltinAIRuntimeFeedback(presentRuntimeMessage(installation.message));
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error || '');
      setBuiltinAIRuntimeFeedback(message.includes('没有可用') || message.includes('发布')
        ? '当前没有可用的 HiMind AI 安装包'
        : `HiMind AI${runtimeActionLabel(operation)}失败，请稍后重试`);
    } finally {
      setBuiltinAIRuntimeBusy(false);
    }
  };
  const installLocalBuiltinAIRuntime = async () => {
    if (builtinAIRuntimeBusy || runtimeWorking) return;
    try {
      const picked = await agentApi.pickRuntimeManifest();
      if (!picked.path) return;
      await startBuiltinAIRuntimeOperation('local', picked.path);
    } catch {
      setBuiltinAIRuntimeFeedback('未选择或无法读取本地安装包');
    }
  };
  const refreshConnectors = async () => {
    setConnectorFeedback('');
    try {
      setConnectorStates(await agentApi.connectorStates());
    } catch {
      setConnectorFeedback('暂时无法读取连接器状态');
    }
  };
  const refreshGithubAccount = async () => {
    try {
      const account = await agentApi.githubDistributionAccount();
      setGithubAccount(account);
      // 撤销授权后 client_id 仍然保留，再次授权时不必重新向管理员索取。
      setGithubClientId(previous => previous || account.app_client_id || '');
      if (account.authorized && account.auth_kind !== 'app') setGithubMethod('pat');
    } catch {
      setGithubFeedback('暂时无法读取 GitHub 授权状态');
    }
  };
  const saveGithubToken = async () => {
    if (!githubToken.trim()) return;
    setGithubBusy(true);
    setGithubFeedback('');
    try {
      const account = await agentApi.setGithubDistributionAccount(githubToken.trim());
      setGithubAccount(account);
      setGithubToken('');
      setGithubFeedback(`已授权 GitHub 账号 ${account.login}`);
    } catch (error) {
      setGithubFeedback(failureText(error, 'GitHub 授权失败'));
      // 校验失败时不把令牌留在界面上，避免明文长时间停在输入框里。
      setGithubToken('');
    } finally {
      setGithubBusy(false);
    }
  };
  const clearGithubToken = async () => {
    setGithubBusy(true);
    setGithubFeedback('');
    try {
      await agentApi.removeGithubDistributionAccount();
      setGithubAccount(await agentApi.githubDistributionAccount());
      setGithubInstallations([]);
      setGithubFeedback('已解除 GitHub 授权');
    } catch (error) {
      setGithubFeedback(failureText(error, '解除 GitHub 授权失败'));
    } finally {
      setGithubBusy(false);
    }
  };
  const startGithubAppAuthorization = async () => {
    setGithubBusy(true);
    setGithubFeedback('');
    try {
      const started = await agentApi.startGithubAppAuthorization(githubClientId.trim() || undefined);
      setGithubClientId(started.client_id);
      setGithubDevice({ client_id: started.client_id, ...started.authorization });
    } catch (error) {
      setGithubFeedback(failureText(error, 'GitHub App 授权失败'));
    } finally {
      setGithubBusy(false);
    }
  };
  const openGithubAuthorizationPage = async () => {
    if (!githubDevice) return;
    try {
      // 优先用带 user_code 的地址：授权页直接预填，用户少一次手输。
      await agentApi.openGithubAuthorizationPage(githubDevice.verification_uri_complete || githubDevice.verification_uri);
    } catch (error) {
      setGithubFeedback(failureText(error, '打开授权页失败'));
    }
  };
  const copyGithubUserCode = async () => {
    if (!githubDevice) return;
    try {
      await navigator.clipboard.writeText(githubDevice.user_code);
      setGithubFeedback('已复制授权码');
    } catch {
      setGithubFeedback('复制失败，请手动输入授权码');
    }
  };
  const cancelGithubAppAuthorization = () => {
    // 设备码最长可挂 15 分钟，用户改主意时要能退出来重填 client_id，不能只能等超时。
    setGithubDevice(null);
    setGithubFeedback('已取消本次授权');
  };
  const loadGithubInstallations = async () => {
    setGithubBusy(true);
    setGithubFeedback('');
    try {
      const result = await agentApi.listGithubAppInstallations();
      setGithubInstallations(result.installations || []);
      if (!(result.installations || []).length) setGithubFeedback('没有可用安装，请先在 GitHub 上把 App 安装到目标账号');
    } catch (error) {
      setGithubFeedback(failureText(error, '读取安装列表失败'));
    } finally {
      setGithubBusy(false);
    }
  };
  const bindGithubInstallation = async (installationId: string) => {
    setGithubBusy(true);
    setGithubFeedback('');
    try {
      setGithubAccount(await agentApi.selectGithubAppInstallation(installationId));
      setGithubInstallations([]);
      setGithubFeedback('已绑定发布账号');
    } catch (error) {
      setGithubFeedback(failureText(error, '绑定发布账号失败'));
    } finally {
      setGithubBusy(false);
    }
  };
  const importGithubAppPrivateKey = async () => {
    setGithubBusy(true);
    setGithubFeedback('');
    try {
      setGithubAccount(await agentApi.importGithubAppPrivateKey());
      setGithubFeedback('私钥已导入');
    } catch (error) {
      const message = failureText(error, '导入私钥失败');
      // 取消选择不是失败，不该在界面上留下红字。
      setGithubFeedback(message.includes('取消') ? '' : message);
    } finally {
      setGithubBusy(false);
    }
  };
  const updateConnector = async (connectorId: string, action: 'enable' | 'disable' | 'revoke' | 'restore') => {
    setConnectorBusy(`${connectorId}:${action}`);
    setConnectorFeedback('');
    try {
      if (action === 'enable') await agentApi.setConnectorEnabled(connectorId, true);
      else if (action === 'disable') await agentApi.setConnectorEnabled(connectorId, false);
      else if (action === 'revoke') await agentApi.revokeConnector(connectorId, 'revoked from Agent settings');
      else await agentApi.restoreConnector(connectorId);
      await refreshConnectors();
      setConnectorFeedback(action === 'revoke' ? '连接器已撤销，相关本地凭据已清理' : '连接器状态已更新');
    } catch (error) {
      setConnectorFeedback(error instanceof Error ? error.message : '连接器操作失败');
    } finally {
      setConnectorBusy('');
    }
  };
  const primaryRuntimeAction = async () => {
    if (!runtimeReady) return startBuiltinAIRuntimeOperation('install');
    if (builtinAIRuntimeInstallation?.update_available) return startBuiltinAIRuntimeOperation('update');
    return checkBuiltinAIRuntimeUpdate();
  };
  const [unityEditorPath, setUnityEditorPath] = useState('');
  const [unityEditorSettings, setUnityEditorSettings] = useState<UnityEditorSettings | null>(null);
  const [unrealEditorPath, setUnrealEditorPath] = useState('');
  const [engineInstallations, setEngineInstallations] = useState<EngineInstallation[]>([]);
  const [editorFeedback, setEditorFeedback] = useState('');
  const [editorSaving, setEditorSaving] = useState(false);
  const [pendingRemoteRuntimeUnrestricted, setPendingRemoteRuntimeUnrestricted] = useState<RemoteExecutionSettings | null>(null);
  const [pendingApprovalProfile, setPendingApprovalProfile] = useState<'trusted' | 'full_access' | null>(null);
  const [remoteClientDrafts, setRemoteClientDrafts] = useState<Record<RemoteClientVendor, string>>({ sunlogin: '', todesk: '' });
  const remoteClientDraftsInitialized = useRef(false);
  const [remoteClientBusy, setRemoteClientBusy] = useState<RemoteClientVendor | 'detect' | null>(null);
  const [remoteClientFeedback, setRemoteClientFeedback] = useState<Record<RemoteClientVendor, string>>({ sunlogin: '', todesk: '' });
  const [skillSyncMode, setSkillSyncMode] = useState<'copy' | 'symlink'>('copy');
  const [skillTargetRoot, setSkillTargetRoot] = useState('');
  const [skillSyncBusy, setSkillSyncBusy] = useState(false);
  const [skillFeedback, setSkillFeedback] = useState('');

  // 切换写入方式会重写每个技能在每个 AI 工具里的副本，所以按"改设置 + 重新生成 + 汇报结果"三步走。
  async function chooseSkillSyncMode(mode: 'copy' | 'symlink') {
    if (mode === skillSyncMode || skillSyncBusy) return;
    setSkillSyncBusy(true);
    setSkillFeedback('');
    try {
      await agentApi.setSkillSyncMode(mode);
      await agentApi.syncCodexSkills();
      setSkillSyncMode(mode);
      const status = await agentApi.codexSkillStatus();
      setSkillTargetRoot(status.target_root || '');
      setSkillFeedback(mode === 'copy' ? '已改为复制文件，并重新生成各工具的副本' : '已改为链接文件，并重新生成各工具的链接');
    } catch (error) {
      setSkillFeedback(failureText(error, '更新技能写入方式失败'));
    } finally {
      setSkillSyncBusy(false);
    }
  }
  useEffect(() => {
    setUnityEditorSettings(settings?.editors || null);
    setUnityEditorPath(settings?.editors?.unity_editor_path || '');
    setUnrealEditorPath(settings?.editors?.unreal?.unreal_editor_path || '');
  }, [settings?.editors]);
  // 本机引擎清单要扫安装目录，只在真正打开「开发工具」时读一次，避免每次设置刷新都扫盘。
  useEffect(() => {
    if (section !== 'tooling' || settingsSectionTabFor(section, tab) !== 'tools') return;
    let cancelled = false;
    void agentApi.engineInstallations()
      .then(items => { if (!cancelled) setEngineInstallations(items); })
      .catch(() => { if (!cancelled) setEngineInstallations([]); });
    return () => { cancelled = true; };
  }, [section, tab]);
  // GitHub 授权状态只在账号页出现时才读取，避免设置窗口打开就产生额外 IPC。
  useEffect(() => {
    if (section !== 'accounts') return;
    void refreshGithubAccount();
  }, [section]);
  // 设备流：拿到 user_code 后按 GitHub 给的间隔轮询，直到用户授权、拒绝或超时。
  // 取消切换小节就停止轮询，避免关掉设置窗口后还在后台打点。
  useEffect(() => {
    if (section !== 'accounts' || !githubDevice) return;
    let cancelled = false;
    let timer: number | undefined;
    let intervalMs = Math.max(2, githubDevice.interval || 5) * 1000;
    const deadline = Date.now() + Math.max(60, githubDevice.expires_in || 900) * 1000;
    const tick = async () => {
      try {
        const result = await agentApi.pollGithubAppAuthorization(githubDevice.client_id, githubDevice.device_code);
        if (cancelled) return;
        if (result.state === 'authorized') {
          setGithubDevice(null);
          setGithubAccount(await agentApi.githubDistributionAccount());
          const installations = result.installations || [];
          setGithubInstallations(installations);
          setGithubFeedback(installations.length ? '已完成授权，请选择发布账号' : '已完成授权，但还没有可用安装，请先在 GitHub 上安装这个 App');
          return;
        }
        if (result.state === 'expired' || Date.now() > deadline) {
          setGithubDevice(null);
          setGithubFeedback('授权超时，请重新开始');
          return;
        }
        if (result.state === 'denied') {
          setGithubDevice(null);
          setGithubFeedback('授权被取消');
          return;
        }
        if (result.state === 'slow_down') intervalMs += 5000;
        timer = window.setTimeout(() => void tick(), intervalMs);
      } catch (error) {
        if (!cancelled) {
          setGithubDevice(null);
          setGithubFeedback(failureText(error, 'GitHub App 授权失败'));
        }
      }
    };
    timer = window.setTimeout(() => void tick(), intervalMs);
    return () => {
      cancelled = true;
      if (timer) window.clearTimeout(timer);
    };
  }, [section, githubDevice]);
  // 技能安装方式是全局配置，只有进入「技能」小节时才读取。
  useEffect(() => {
    if (section !== 'tooling' || settingsSectionTabFor(section, tab) !== 'skills') return;
    let active = true;
    void (async () => {
      try {
        const [settingsValue, status] = await Promise.all([agentApi.skillSyncSettings(), agentApi.codexSkillStatus()]);
        if (!active) return;
        setSkillSyncMode(settingsValue.mode);
        setSkillTargetRoot(status.target_root || '');
      } catch (error) {
        if (active) setSkillFeedback(failureText(error, '读取技能设置失败'));
      }
    })();
    return () => { active = false; };
  }, [section, tab]);
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
    if (section === 'services' && settingsSectionTabFor(section, tab) === 'connectors') void refreshConnectors();
  }, [section, tab]);
  useEffect(() => {
    if (section !== 'diagnostics' || settingsSectionTabFor(section, tab) !== 'backup' || backupScope) return;
    let disposed = false;
    void agentApi.backupScope()
      .then(scope => { if (!disposed) setBackupScope(scope); })
      .catch(() => { if (!disposed) setBackupFeedback('暂时无法读取备份范围'); });
    return () => { disposed = true; };
  }, [section, tab, backupScope]);
  const backupMinPassphrase = backupScope?.minPassphraseChars ?? 8;
  const exportBackup = async () => {
    // 只要本机存过账号或凭据，导出就必须带口令，否则包里的凭据是空的。
    // 在这里挡住，用户不必先选完保存位置才知道要填口令。
    const passphrase = backupPassphrase.trim();
    if (!passphrase) {
      setBackupFeedback(`先设置一个至少 ${backupMinPassphrase} 位的口令，账号与凭据会用它加密`);
      return;
    }
    if (passphrase.length < backupMinPassphrase) {
      setBackupFeedback(`口令至少 ${backupMinPassphrase} 位`);
      return;
    }
    setBackupBusy('export');
    setBackupFeedback('');
    try {
      const result = await agentApi.exportBackup(passphrase, backupIncludeDeviceIdentity);
      if (result.canceled) {
        setBackupFeedback('已取消导出');
        return;
      }
      setBackupExportReport(result.report);
      setBackupInspectReport(null);
      setBackupRestoreReport(null);
      setBackupFeedback(`已写入 ${result.report.path}`);
    } catch (error) {
      setBackupFeedback(failureText(error, '导出备份包失败'));
    } finally {
      setBackupBusy('');
    }
  };
  const inspectBackup = async () => {
    setBackupBusy('inspect');
    setBackupFeedback('');
    try {
      const result = await agentApi.inspectBackup();
      if (result.canceled) {
        setBackupFeedback('已取消查看');
        return;
      }
      setBackupInspectReport(result.report);
      setBackupExportReport(null);
      setBackupRestoreReport(null);
    } catch (error) {
      setBackupFeedback(failureText(error, '无法读取备份包'));
    } finally {
      setBackupBusy('');
    }
  };
  const restoreBackup = async () => {
    setPendingBackupRestore(false);
    setBackupBusy('restore');
    setBackupFeedback('');
    try {
      const result = await agentApi.importBackup(null, backupPassphrase.trim() || null);
      if (result.canceled) {
        setBackupFeedback('已取消恢复');
        return;
      }
      setBackupRestoreReport(result.report);
      setBackupExportReport(null);
      setBackupInspectReport(null);
      setBackupFeedback(`已恢复 ${result.report.restored.length} 个文件，重启 Agent 后生效`);
    } catch (error) {
      setBackupFeedback(failureText(error, '恢复备份失败'));
    } finally {
      setBackupBusy('');
    }
  };
  async function chooseEngineEditor(engine: 'unity' | 'unreal') {
    const result = await agentApi.pickEngineEditor(engine);
    if (!result.path) return;
    if (engine === 'unity') setUnityEditorPath(result.path);
    else setUnrealEditorPath(result.path);
    // 选择路径即保存，不再要求用户再点一次保存按钮。
    await saveEngineEditor(engine, result.path);
  }

  async function saveEngineEditor(engine: 'unity' | 'unreal', path: string) {
    setEditorSaving(true);
    setEditorFeedback('');
    const label = engine === 'unity' ? 'Unity 编辑器' : 'Unreal 编辑器';
    try {
      const result = await agentApi.saveEngineEditor(engine, path);
      onUnityEditorSettingsChange(result);
      setUnityEditorSettings(result);
      setUnityEditorPath(result.unity_editor_path);
      setUnrealEditorPath(result.unreal?.unreal_editor_path || '');
      const source = engine === 'unity' ? result.source : result.unreal?.source;
      setEditorFeedback(path ? `${label}已保存` : ['environment', 'discovered'].includes(source || '') ? `${label}已恢复默认` : `${label}已清除`);
    } catch {
      setEditorFeedback(engine === 'unity' ? '无法保存，请确认 Unity.exe 路径' : '无法保存，请确认 UnrealEditor.exe 路径');
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
        await saveRemoteClient(vendor, result.path);
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

  if (!settings || !remoteExecutionSettings || !loginState) return <div className="page-loading"><BusyIndicator size={15} />正在读取应用设置</div>;
  const configured = loginState.status === 'credentials_configured';
  const editorState = unityEditorSettings || settings.editors;
  const unrealState = editorState?.unreal;
  const editorDirty = unityEditorPath.trim() !== (editorState?.unity_editor_path || '');
  const unrealDirty = unrealEditorPath.trim() !== (unrealState?.unreal_editor_path || '');
  const editorStatus = editorState?.valid && unrealState?.valid ? '可用' : editorState?.valid || unrealState?.valid ? '部分可用' : '未配置';
  const engineSourceLabel = (source?: string) => source === 'agent' ? '自定义' : source === 'environment' ? '团队默认' : source === 'discovered' ? '本机安装' : '未设置';
  const updateRemoteExecution = (patch: Partial<RemoteExecutionSettings>) => {
    const next = { ...remoteExecutionSettings, ...patch };
    const enteringFullAccess = next.access_mode === 'full_access'
      && (remoteExecutionSettings.access_mode !== 'full_access' || (!remoteExecutionSettings.enabled && next.enabled));
    if (enteringFullAccess) setPendingRemoteRuntimeUnrestricted(next);
    else onRemoteExecutionChange(next);
  };
  const approvalProfile = (settings.profile === 'focus' ? 'balanced' : settings.profile || 'balanced') as ApprovalProfile;
  const profileOption = APPROVAL_PROFILE_OPTIONS.find(option => option.value === approvalProfile) || APPROVAL_PROFILE_OPTIONS[1];
  const exactRuleCount = exactApprovalRules.length;
  const effectiveModes = { ...fallbackEffectiveModes(approvalProfile, settings.rules || {}), ...(settings.effective_modes || {}) };
  const changeApprovalProfile = (next: string) => {
    if (next === 'trusted' || next === 'full_access') {
      setPendingApprovalProfile(next);
      return;
    }
    onApprovalProfileChange(next, next === 'trusted');
  };
  const sectionMeta = settingsSectionMeta(section);
  const sectionTabs = settingsSectionTabs(section);
  const activeTab = settingsSectionTabFor(section, tab);
  return (
    <>
      <PageHeader title={sectionMeta.label} description={sectionMeta.description} />
        <div className="settings-content">
          {sectionTabs.length ? <div className="settings-tabs" role="tablist" aria-label={`${sectionMeta.label}分类`}>
            {sectionTabs.map(item => <button
              key={item.key}
              type="button"
              role="tab"
              aria-selected={activeTab === item.key}
              className={activeTab === item.key ? 'active' : ''}
              onClick={() => onTabChange?.(item.key)}
            >{item.label}</button>)}
          </div> : null}

          {section === 'automation' ? <>
            <section className="card settings-section">
              <div className="card-header"><span>远程任务</span><Pill kind={remoteExecutionSettings.enabled ? 'success' : 'neutral'}>{remoteExecutionSettings.enabled ? '已启用' : '已关闭'}</Pill></div>
              <div className="card-body setting-list">
                <SettingRow title="接受远程任务" description="只接收当前 HiMind 账号发给这台电脑的任务"><label className="toggle"><input type="checkbox" checked={remoteExecutionSettings.enabled} onChange={event => updateRemoteExecution({ enabled: event.target.checked })} /><span className="slider"></span></label></SettingRow>
                <SettingRow title="远程任务访问范围" description={remoteExecutionSettings.enabled ? '限制可访问的文件范围；不改审批设置' : '启用远程任务后生效'}>
                  <select aria-label="远程任务访问范围" disabled={!remoteExecutionSettings.enabled} value={remoteExecutionSettings.access_mode} onChange={event => updateRemoteExecution({ access_mode: event.target.value as RemoteExecutionSettings['access_mode'] })}>
                    <option value="exhibit_linked">仅限项目目录（推荐）</option>
                    <option value="full_access">允许访问本机全部文件（高风险）</option>
                  </select>
                </SettingRow>
                <SettingRow title="执行工具" description={remoteExecutionSettings.enabled ? '自动模式会选择本机可用的 AI 工具' : '启用远程任务后生效'}>
                  <select aria-label="远程任务执行工具" disabled={!remoteExecutionSettings.enabled} value={remoteExecutionSettings.default_provider} onChange={event => updateRemoteExecution({ default_provider: event.target.value as RemoteExecutionSettings['default_provider'] })}>
                    <option value="himind.builtin">{localRuntimeMeta('himind.builtin').name}（推荐）</option><option value="auto">自动选择可用工具</option><option value="personal.codex">{localRuntimeMeta('personal.codex').name}</option><option value="personal.github-copilot">{localRuntimeMeta('personal.github-copilot').name}</option>
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
                    <strong>HiMind AI</strong>
                   <span>{presentRuntimeMessage(builtinAIRuntimeInstallation?.message || builtinAIRuntimeStatus?.message) || '正在检查 HiMind AI 状态。'}</span>
                  </div>
                  <div className="actions-row runtime-actions">
                    <button className="btn btn-primary" disabled={builtinAIRuntimeBusy || builtinAIRuntimeCheckBusy || runtimeWorking} onClick={() => void primaryRuntimeAction()}>
                      {builtinAIRuntimeBusy || builtinAIRuntimeCheckBusy || runtimeWorking ? <BusyIndicator size={15} /> : runtimeReady && builtinAIRuntimeInstallation?.update_available ? <Download size={15} /> : runtimeReady ? <RefreshCw size={15} /> : <Download size={15} />}
                      {runtimeWorking ? `${runtimeActionLabel(builtinAIRuntimeInstallation?.operation || 'install')}中 ${builtinAIRuntimeInstallation?.progress_percent || 0}%` : !runtimeReady ? '安装 HiMind AI' : builtinAIRuntimeInstallation?.update_available ? `更新到 v${builtinAIRuntimeInstallation.available_version}` : builtinAIRuntimeCheckBusy ? '检查中' : '检查更新'}
                    </button>
                    <ActionMenu label="更多" icon={<MoreHorizontal size={16} />} title="HiMind AI 运行时的更多操作">{close => <>
                      <ActionMenuItem icon={<FolderOpen size={15} />} label="从本地安装包安装" disabled={builtinAIRuntimeBusy || runtimeWorking} onClick={() => { void installLocalBuiltinAIRuntime(); close(); }} />
                      {runtimeReady ? <ActionMenuItem icon={<Wrench size={15} />} label="修复 HiMind AI" disabled={builtinAIRuntimeBusy || runtimeWorking} onClick={() => { void startBuiltinAIRuntimeOperation('repair'); close(); }} /> : null}
                      {runtimeReady ? <ActionMenuItem danger icon={<Trash2 size={15} />} label="卸载 HiMind AI" disabled={builtinAIRuntimeBusy || runtimeWorking} onClick={() => { setPendingRuntimeUninstall(true); close(); }} /> : <ActionMenuItem icon={<ScanSearch size={15} />} label="重新检测" disabled={builtinAIRuntimeBusy || runtimeWorking} onClick={() => { void refreshBuiltinAIRuntime(); close(); }} />}
                    </>}</ActionMenu>
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
              <div className="card-header"><span>操作审批</span><Pill kind={approvalProfile === 'trusted' || approvalProfile === 'full_access' ? 'warn' : approvalProfile === 'silent_deny' ? 'neutral' : 'success'}>{profileOption.label}</Pill></div>
              <div className="approval-settings-body">
                <div className="approval-identity-row">
                  <div><strong>授权归属</strong><span>{independentMode ? '审批设置只保存在本机' : settings.owner_user_id ? '审批设置与当前工作台账号和这台电脑绑定' : '当前审批设置仅保存在本机'}</span></div>
                  <Pill kind={independentMode || settings.owner_user_id ? 'success' : 'neutral'}>{independentMode ? '未对接工作台' : settings.owner_user_id ? '已绑定账号' : '本机模式'}</Pill>
                </div>
                {independentMode ? <div className="security-note compact approval-independent-note"><ShieldCheck size={16} /><span>未对接 AI 工作台时，记录与提醒只存在本机；其他工具直接执行的操作不经过这里。</span></div> : null}
              <div className="security-note compact approval-independent-note"><LockKeyhole size={16} /><span>受控操作不再询问，工作流确认步骤也跳过；工作台侧授权与远程目录限制不受影响。</span></div>

                <div className="approval-settings-group">
                  <div className="approval-group-heading"><strong>什么时候需要我确认</strong><span>单项例外会优先于整体设置</span></div>
                  <ChoiceGroup className="approval-profile-grid" label="操作审批确认程度" value={approvalProfile} options={APPROVAL_PROFILE_OPTIONS} onChange={changeApprovalProfile} />
                  {(approvalProfile === 'trusted' || approvalProfile === 'full_access') && (settings.risk_acknowledged ? (
                    <div className="approval-trust-expired"><ShieldAlert size={14} />{approvalProfile === 'full_access' ? '完全放行授权' : '完全信任授权'}生效中{settings.risk_acknowledged_duration_seconds ? `，剩余约 ${formatTrustRemaining(settings.risk_acknowledged_remaining_seconds)}` : '（永久，直到撤销）'}</div>
                  ) : (
                    <div className="approval-trust-expired"><ShieldAlert size={14} />{approvalProfile === 'full_access' ? '完全放行授权' : '完全信任授权'}已过期或未确认，请重新确认授权时长</div>
                  ))}
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
                    <ApprovalEffectiveItem icon={ShieldX} label="最高风险操作" mode={effectiveModes.system} />
                  </div>
                </div>

                <details className="approval-advanced">
                  <summary><span><strong>高级设置与单项例外</strong><small>按操作类别或功能 ID 精确调整</small></span><ChevronDown size={16} /></summary>
                  <div className="approval-advanced-body setting-list">
                    <SettingRow title="远程协助" description="由运维工作台发起的远程连接"><Pill kind="success">自动允许</Pill></SettingRow>
                    <SettingRow title="文件上传" description="代码、清单表和制品上传到本机"><ApprovalRuleChoice label="文件上传" value={approvalRuleValue(settings, 'upload_code')} onChange={mode => onRuleChange('upload_code', mode)} /></SettingRow>
                    <SettingRow title="查询与预览" description="只读取信息，不修改文件或业务数据"><ApprovalRuleChoice label="查询与预览" value={approvalRuleValue(settings, 'risk:R1')} onChange={mode => onRuleChange('risk:R1', mode)} /></SettingRow>
                    <SettingRow title="新增与普通修改" description="新增记录、更新项目或写入普通文件"><ApprovalRuleChoice label="新增与普通修改" value={approvalRuleValue(settings, 'risk:R2')} onChange={mode => onRuleChange('risk:R2', mode)} /></SettingRow>
                    <SettingRow title="删除、发布与权限变更" description="高风险操作；自动允许仅在完全信任或完全放行下可用"><ApprovalRuleChoice label="删除、发布与权限变更" value={approvalRuleValue(settings, 'risk:R3')} autoApproveDisabled={!['trusted', 'full_access'].includes(approvalProfile)} onChange={mode => onRuleChange('risk:R3', mode)} /></SettingRow>
                    <SettingRow title="系统级操作" description="仅在完全放行时可设为自动允许"><ApprovalRuleChoice label="系统级操作" value={approvalRuleValue(settings, 'risk:R4')} autoApproveDisabled={approvalProfile !== 'full_access'} onChange={mode => onRuleChange('risk:R4', mode)} /></SettingRow>
                <SettingRow title="系统保护边界" description="系统目录、应用数据、安装目录、磁盘根与越界路径"><Pill kind="danger">始终阻止</Pill></SettingRow>
                    <SettingRow title="其他受控操作" description="没有单独分类但仍需审批的操作"><ApprovalRuleChoice label="其他受控操作" value={approvalRuleValue(settings, 'controlled_operation')} onChange={mode => onRuleChange('controlled_operation', mode)} /></SettingRow>
                    <SettingRow title="未分类普通操作" description="尚未分类的非高风险操作"><ApprovalRuleChoice label="未分类普通操作" value={approvalRuleValue(settings, '*')} onChange={mode => onRuleChange('*', mode)} /></SettingRow>
                    {exactApprovalRules.map(([requestType, mode]) => <SettingRow key={requestType} title={requestType} description="功能单项例外"><ApprovalRuleChoice label={requestType} value={mode as ApprovalRuleMode} autoApproveDisabled={(requestType === 'risk:R4' && approvalProfile !== 'full_access') || (isHighRiskRuleKey(requestType) && !['trusted', 'full_access'].includes(approvalProfile))} onChange={next => onRuleChange(requestType, next)} /></SettingRow>)}
                    <SettingRow title="新增功能例外" description="按功能 ID 设置精确授权">
                      <div className="approval-rule-editor">
                        <input aria-label="功能 ID" placeholder="例如 ai.client.import" value={approvalCapabilityId} onChange={event => setApprovalCapabilityId(event.target.value)} />
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

          {section === 'services' && activeTab === 'remote-clients' ? <section className="card settings-section remote-client-settings">
              <div className="card-header"><span>远程控制</span><div className="card-header-actions"><button type="button" className="btn btn-icon" title="重新检测本机客户端" aria-label="重新检测本机客户端" disabled={remoteClientBusy !== null} onClick={() => void detectRemoteClients()}>{remoteClientBusy === 'detect' ? <BusyIndicator size={16} /> : <ScanSearch size={16} />}</button></div></div>
            <div className="remote-client-body">
              <div className="remote-client-list">
                {REMOTE_CLIENT_OPTIONS.map(option => <RemoteClientCard key={option.vendor} option={option} status={remoteClients?.items.find(item => item.vendor === option.vendor)} path={remoteClientDrafts[option.vendor]} busy={remoteClientBusy === option.vendor} feedback={remoteClientFeedback[option.vendor]} onPathChange={path => { setRemoteClientDrafts(current => ({ ...current, [option.vendor]: path })); setRemoteClientFeedback(current => ({ ...current, [option.vendor]: '' })); }} onPick={() => void chooseRemoteClient(option.vendor)} onSave={() => void saveRemoteClient(option.vendor)} onClear={() => void saveRemoteClient(option.vendor, '')} />)}
              </div>
            </div>
          </section> : null}

          {section === 'accounts' ? <>
          <WorkbenchConnectionsPanel
            snapshot={workbenchConnections}
            error={workbenchConnectionsError || ''}
            identity={identity}
            authorization={workbenchAuthorization}
            busyId={workbenchBusyId}
            onRefresh={onRefreshWorkbenchConnections}
            onAdd={onAddWorkbenchConnection}
            onRename={onRenameWorkbenchConnection}
            onRemove={onRemoveWorkbenchConnection}
            onSwitch={onSwitchWorkbenchConnection}
            onEnroll={onEnrollWorkbenchConnection}
            onProbe={onProbeWorkbenchConnection}
            onAuthorize={onAuthorizeWorkbenchConnection}
            onRevoke={onRevokeAuthorization}
          />
          <section className="card settings-section settings-credentials">
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
                <div className="actions-row svn-connection-actions"><button className="btn" disabled={svnTesting} onClick={onTestSvnConnection}>{svnTesting ? <><BusyIndicator size={14} />测试中</> : '测试'}</button><button className="btn" disabled={svnTesting} onClick={onOpenSvnModal}>更新账号</button><button className="btn btn-danger-quiet" disabled={svnTesting} onClick={onRemoveSvnConnection}>清除</button></div>
              </div> : <div className="account-row"><div className="account-icon"><Database size={17} /></div><div><span>公司 SVN</span><strong>尚未配置个人账号</strong></div><button className="btn btn-primary" onClick={onOpenSvnModal}>配置账号</button></div>}
              <div className="security-note compact"><ShieldCheck size={16} /><span>密码只保存在当前 Windows 用户的本地加密存储中。</span></div>
            </div>
            <div className="credential-section">
              <div className="credential-heading"><span>GitHub 账号</span><Pill kind={githubAccount?.authorized ? 'success' : 'neutral'}>{githubAccount?.authorized ? '已授权' : '未授权'}</Pill></div>
              {githubAccount?.authorized ? <div className="account-row">
                <div className="account-icon"><Github size={17} /></div>
                <div>
                  <span>{githubAccount.auth_kind === 'app' ? 'GitHub App' : '个人访问令牌'}</span>
                  <strong>{githubAccount.login || '已授权'}</strong>
                  <small>{githubAccount.auth_kind === 'app'
                    ? [githubAccount.installation_account ? `发布账号 ${githubAccount.installation_account}` : '尚未绑定发布账号', githubAccount.private_key_configured ? '' : '尚未导入私钥'].filter(Boolean).join(' · ')
                    : '扩展版本可发布为 GitHub Release'}</small>
                </div>
                <div className="actions-row">
                  {githubAccount.auth_kind === 'app' && !githubAccount.private_key_configured
                    ? <button className="btn btn-primary" disabled={githubBusy} onClick={() => void importGithubAppPrivateKey()}>{githubBusy ? <BusyIndicator size={15} /> : <KeyRound size={15} />}导入私钥</button>
                    : null}
                  {githubAccount.auth_kind === 'app' && githubAccount.private_key_configured
                    ? <button className="btn" disabled={githubBusy} onClick={() => void loadGithubInstallations()}>{githubBusy ? <BusyIndicator size={15} /> : null}更换发布账号</button>
                    : null}
                  <button className="btn btn-danger-quiet" disabled={githubBusy} onClick={() => void clearGithubToken()}>{githubBusy ? <BusyIndicator size={15} /> : <Trash2 size={15} />}解除授权</button>
                </div>
              </div> : <>
                <div className="segmented-control" role="group" aria-label="GitHub 授权方式">
                  <button type="button" className={githubMethod === 'app' ? 'active' : ''} disabled={githubBusy || Boolean(githubDevice)} onClick={() => setGithubMethod('app')}>GitHub App</button>
                  <button type="button" className={githubMethod === 'pat' ? 'active' : ''} disabled={githubBusy || Boolean(githubDevice)} onClick={() => setGithubMethod('pat')}>个人令牌</button>
                </div>
                {githubMethod === 'app' ? <div className="field-group" style={{ marginTop: 12 }}>
                  {githubDevice ? <>
                    <label className="field-label">在 GitHub 上输入这个授权码</label>
                    <div className="actions-row">
                      <code className="device-code">{githubDevice.user_code}</code>
                      <button type="button" className="btn" onClick={() => void copyGithubUserCode()}>复制</button>
                      <button type="button" className="btn btn-primary" onClick={() => void openGithubAuthorizationPage()}><ExternalLink size={15} />打开授权页</button>
                      <button type="button" className="btn btn-quiet" onClick={cancelGithubAppAuthorization}>取消</button>
                    </div>
                <p className="field-hint">「打开授权页」会带上授权码；授权完成后本页自动继续，无需确认。</p>
                  </> : <>
                    <label className="field-label" htmlFor="github-app-client-id">App client_id</label>
                    <input
                      id="github-app-client-id"
                      autoComplete="off"
                      value={githubClientId}
                      disabled={githubBusy}
                      placeholder="Iv23li..."
                      onChange={event => { setGithubClientId(event.target.value); setGithubFeedback(''); }}
                    />
                    <p className="field-hint">组织注册 App 时生成，授权一次之后无需再填。</p>
                    <div className="actions-row" style={{ marginTop: 8 }}>
                      <button type="button" className="btn btn-primary" disabled={githubBusy} onClick={() => void startGithubAppAuthorization()}>{githubBusy ? <BusyIndicator size={15} /> : <LogIn size={15} />}开始授权</button>
                    </div>
                  </>}
                </div> : <div className="field-group" style={{ marginTop: 12 }}>
                  <label className="field-label" htmlFor="github-distribution-token">个人访问令牌</label>
                  <input
                    id="github-distribution-token"
                    type="password"
                    autoComplete="off"
                    value={githubToken}
                    disabled={githubBusy}
                    placeholder="细粒度令牌，需要 Contents: Read and write"
                    onChange={event => { setGithubToken(event.target.value); setGithubFeedback(''); }}
                  />
                  <p className="field-hint">临时方案：令牌会跟随账号有效期，App 授权更省事。</p>
                  <div className="actions-row" style={{ marginTop: 8 }}>
                    <button type="button" className="btn btn-primary" disabled={githubBusy || !githubToken.trim()} onClick={() => void saveGithubToken()}>{githubBusy ? <BusyIndicator size={15} /> : <LogIn size={15} />}校验并授权</button>
                  </div>
                </div>}
              </>}
              {githubInstallations.length ? <div className="field-group" style={{ marginTop: 12 }}>
                <label className="field-label">选择发布账号</label>
                {githubInstallations.map(item => (
                  <div key={item.id} className="account-row">
                    <div className="account-icon"><Github size={16} /></div>
                    <div>
                      <span>{item.account_type === 'Organization' ? '组织安装' : '个人安装'}</span>
                      <strong>{item.account}</strong>
                      <small>{item.repository_selection === 'all' ? '全部仓库' : '部分仓库'}</small>
                    </div>
                    <div className="actions-row"><button type="button" className="btn" disabled={githubBusy} onClick={() => void bindGithubInstallation(item.id)}>绑定</button></div>
                  </div>
                ))}
              </div> : null}
              <div className="security-note compact"><ShieldCheck size={16} /><span>凭据经 DPAPI 加密保存在本机，只用于创建 tag 与 Release，不写入日志或控制面。</span></div>
              {githubFeedback ? <div className="inline-feedback visible" role="status">{githubFeedback}</div> : null}
            </div>
          </section>
          </> : null}

          {section === 'services' && activeTab === 'connectors' ? <section className="card settings-section">
            <div className="card-header">
              <span>工作流连接器</span>
              <Pill kind={connectorStates.some(item => item.revoked || !item.enabled) ? 'warn' : connectorStates.length ? 'success' : 'neutral'}>{connectorStates.length}</Pill>
            </div>
            <div className="card-body setting-list">
              {connectorStates.length ? connectorStates.map(connector => (
                <div key={connector.id} className="account-row">
                  <div className="account-icon"><PlugZap size={17} /></div>
                  <div>
                    <span>{connector.name}</span>
                    <strong>{connectorAvailabilityLabel(connector.availability)}</strong>
                    <small>v{connector.version} · {connector.credential_count} 个本地凭据 · {connector.source === 'dashboard' ? '组织管理' : '本机管理'}</small>
                  </div>
                  <Pill kind={connector.revoked ? 'danger' : connector.enabled ? 'success' : 'warn'}>{connector.revoked ? '已撤销' : connector.enabled ? '已启用' : '已停用'}</Pill>
                  <div className="actions-row">
                    {!connector.revoked ? <button className="btn btn-icon" title={connector.enabled ? '停用连接器' : '启用连接器'} aria-label={connector.enabled ? '停用连接器' : '启用连接器'} disabled={Boolean(connectorBusy)} onClick={() => void updateConnector(connector.id, connector.enabled ? 'disable' : 'enable')}><Power size={15} /></button> : null}
                    {connector.revoked ? <button className="btn" disabled={Boolean(connectorBusy)} onClick={() => void updateConnector(connector.id, 'restore')}>恢复</button> : <button className="btn btn-danger-quiet" disabled={Boolean(connectorBusy)} onClick={() => { void confirm({ title: `撤销连接器「${connector.name}」？`, description: '本地保存的连接器凭据会被删除，需要重新授权才能再用。', confirmText: '撤销' }).then(accepted => { if (accepted) void updateConnector(connector.id, 'revoke'); }); }}>撤销</button>}
                  </div>
                </div>
              )) : <div className="unity-editor-source"><div><span>工作流连接器</span><strong>暂无已安装工作流连接器</strong></div><small>安装带有连接器的工作流后会显示在这里。</small></div>}
              {connectorFeedback ? <div className="inline-feedback visible" role="status">{connectorFeedback}</div> : null}
            </div>
          </section> : null}

          {section === 'tooling' && activeTab === 'skills' ? <section className="card settings-section">
            <div className="card-header">技能写入方式</div>
            <div className="card-body skill-settings-body">
              <div className="actions-row">
                <button type="button" className={skillSyncMode === 'copy' ? 'btn btn-primary' : 'btn'} disabled={skillSyncBusy} onClick={() => void chooseSkillSyncMode('copy')}><Check size={15} />复制文件</button>
                <button type="button" className={skillSyncMode === 'symlink' ? 'btn btn-primary' : 'btn'} disabled={skillSyncBusy} onClick={() => void chooseSkillSyncMode('symlink')}><Link2 size={15} />链接文件</button>
                {skillSyncBusy ? <BusyIndicator size={15} /> : null}
              </div>
              <p className="field-hint">
                {skillSyncMode === 'copy'
                  ? '复制：每个 AI 工具目录下各存一份独立副本，日常用这个。'
                  : '链接：工具目录指向本机技能库，改动即时生效；部分系统需先开启开发者模式。'}
              </p>
              <div className="actions-row">
                <button className="btn" type="button" disabled={!skillTargetRoot} title={skillTargetRoot || '技能目录尚未就绪'} onClick={() => void agentApi.openFolder(skillTargetRoot)}><FolderOpen size={15} />打开技能目录</button>
                <code className="skill-target-path">{skillTargetRoot || '技能目录尚未就绪'}</code>
              </div>
              <p className="field-hint">默认装到全局，所有工具可用；要单独装一份，用技能详情里的「安装到指定目录…」。</p>
              {skillFeedback ? <div className="inline-feedback visible" role="status">{skillFeedback}</div> : null}
            </div>
          </section> : null}

          {section === 'tooling' && activeTab === 'tools' ? <section className="card settings-section unity-editor-card">
            <div className="card-header"><span>引擎与编辑器</span><Pill kind={editorState?.valid || unrealState?.valid ? 'success' : 'warn'}>{editorStatus}</Pill></div>
            <div className="unity-editor-body">
              <div className="engine-editor-row">
                <div className="engine-editor-head"><strong>Unity</strong><Pill kind={editorState?.valid ? 'success' : 'warn'}>{engineSourceLabel(editorState?.source)}</Pill></div>
                <div className="unity-editor-input">
                  <input aria-label="Unity.exe 路径" value={unityEditorPath} onChange={event => { setUnityEditorPath(event.target.value); setEditorFeedback(''); }} onBlur={() => { if (editorDirty && unityEditorPath.trim()) void saveEngineEditor('unity', unityEditorPath); }} placeholder="请选择 Unity.exe" />
                  <button type="button" className="btn btn-icon" title="选择 Unity.exe" aria-label="选择 Unity.exe" disabled={editorSaving} onClick={() => void chooseEngineEditor('unity')}><FolderOpen size={16} /></button>
                  <button type="button" className="btn btn-icon" title="恢复 Unity 默认" aria-label="恢复 Unity 默认" disabled={editorSaving || (!editorDirty && editorState?.source !== 'agent')} onClick={() => void saveEngineEditor('unity', '')}><RotateCcw size={16} /></button>
                </div>
              </div>
              <div className="engine-editor-row">
                <div className="engine-editor-head"><strong>Unreal</strong><Pill kind={unrealState?.valid ? 'success' : 'warn'}>{engineSourceLabel(unrealState?.source)}</Pill></div>
                <div className="unity-editor-input">
                  <input aria-label="UnrealEditor.exe 路径" value={unrealEditorPath} onChange={event => { setUnrealEditorPath(event.target.value); setEditorFeedback(''); }} onBlur={() => { if (unrealDirty && unrealEditorPath.trim()) void saveEngineEditor('unreal', unrealEditorPath); }} placeholder="请选择 UnrealEditor.exe" />
                  <button type="button" className="btn btn-icon" title="选择 UnrealEditor.exe" aria-label="选择 UnrealEditor.exe" disabled={editorSaving} onClick={() => void chooseEngineEditor('unreal')}><FolderOpen size={16} /></button>
                  <button type="button" className="btn btn-icon" title="恢复 Unreal 默认" aria-label="恢复 Unreal 默认" disabled={editorSaving || (!unrealDirty && unrealState?.source !== 'agent')} onClick={() => void saveEngineEditor('unreal', '')}><RotateCcw size={16} /></button>
                </div>
              </div>
              {engineInstallations.length ? <div className="engine-installation-list">
                <span>本机已安装</span>
                <div>{engineInstallations.map(item => <button type="button" key={`${item.engine}-${item.version}-${item.path}`} className="engine-installation" title={item.path} disabled={editorSaving} onClick={() => void saveEngineEditor(item.engine, item.path)}>{item.engine === 'unity' ? 'Unity' : 'Unreal'} {item.version || '未知版本'}</button>)}</div>
              </div> : null}
              <div className="unity-editor-footer">
                <span className={editorFeedback ? 'inline-feedback visible' : 'inline-feedback'}>{editorFeedback}</span>
              </div>
            </div>
          </section> : null}

          {section === 'diagnostics' && activeTab === 'logs' ? <LogsPage embedded logs={logs} onExport={onExportDiagnostics} /> : null}

          {section === 'diagnostics' && activeTab === 'backup' ? <>
            <section className="card settings-section">
              <div className="card-header"><span>备份包</span><Pill kind="neutral">配置与凭据</Pill></div>
              {/* 口令输入要落在 <form> 里：口令是这一屏的主输入，回车提交给「导出备份包」才是自然动作，
                  同时 Chromium 也不会再判定「密码框不属于任何表单」。 */}
              <form
                onSubmit={event => {
                  event.preventDefault();
                  if (!backupBusy) void exportBackup();
                }}
              >
                <div className="card-body setting-list">
                  <SettingRow title="口令" description={`加密账号与凭据，至少 ${backupMinPassphrase} 位；不写入备份包，忘记无法找回。`}>
                    <input type="password" aria-label="备份包口令" autoComplete="new-password" value={backupPassphrase} placeholder={`至少 ${backupMinPassphrase} 位`} onChange={event => setBackupPassphrase(event.target.value)} />
                  </SettingRow>
                  <SettingRow title="包含设备身份" description="只在本机重装时勾选；换机恢复保留新机器身份。">
                    <label className="toggle"><input type="checkbox" checked={backupIncludeDeviceIdentity} onChange={event => setBackupIncludeDeviceIdentity(event.target.checked)} /><span className="slider"></span></label>
                  </SettingRow>
                </div>
                <div className="card-body actions-row backup-actions">
                  <button type="submit" className="btn btn-primary" disabled={Boolean(backupBusy)}>{backupBusy === 'export' ? <BusyIndicator size={15} /> : <Download size={15} />}导出备份包</button>
                  <button type="button" className="btn" disabled={Boolean(backupBusy)} onClick={() => void inspectBackup()}>{backupBusy === 'inspect' ? <BusyIndicator size={15} /> : <ScanSearch size={15} />}查看备份包</button>
                  <button type="button" className="btn" disabled={Boolean(backupBusy)} onClick={() => setPendingBackupRestore(true)}>{backupBusy === 'restore' ? <BusyIndicator size={15} /> : <RotateCcw size={15} />}从备份包恢复</button>
                </div>
              </form>
              {backupFeedback ? <div className="card-body"><div className="inline-feedback visible" role="status">{backupFeedback}</div></div> : null}
            </section>

            {backupExportReport ? <section className="card settings-section">
              <div className="card-header"><span>已导出</span><Pill kind="success">{formatBackupBytes(backupExportReport.total_bytes)}</Pill></div>
              <div className="software-update-summary">
                <div><span>文件</span><strong>{backupExportReport.file_count} 个</strong></div>
                <div><span>凭据</span><strong>{backupExportReport.credentials} 项</strong></div>
              </div>
              <div className="card-body backup-body">
                <code className="backup-path" title={backupExportReport.path}>{backupExportReport.path}</code>
                <div className="backup-tags">{backupExportReport.categories.map(item => <span className="pill neutral" key={item.category}>{item.category} {item.files}</span>)}</div>
                {backupExportReport.skipped.length ? <BackupDetails summary={`未打包 ${backupExportReport.skipped.length} 项`} items={backupExportReport.skipped.map(item => `${item.path} · ${item.reason}`)} /> : null}
                {backupExportReport.warnings.length ? <BackupDetails summary={`提醒 ${backupExportReport.warnings.length} 条`} items={backupExportReport.warnings} /> : null}
              </div>
            </section> : null}

            {backupInspectReport ? <section className="card settings-section">
              <div className="card-header"><span>备份包内容</span><Pill kind="neutral">v{backupInspectReport.version}</Pill></div>
              <div className="software-update-summary">
                <div><span>导出时间</span><strong>{formatBackupTime(backupInspectReport.created_at)}</strong></div>
                <div><span>导出设备</span><strong>{backupInspectReport.machine || '—'}</strong></div>
                <div><span>文件</span><strong>{backupInspectReport.file_count} 个 · {formatBackupBytes(backupInspectReport.total_bytes)}</strong></div>
                <div><span>凭据</span><strong>{backupInspectReport.credentials} 项{backupInspectReport.needs_passphrase ? ' · 需要口令' : ''}</strong></div>
              </div>
              <div className="card-body backup-body">
                <code className="backup-path" title={backupInspectReport.path}>{backupInspectReport.path}</code>
                <div className="backup-tags">{backupInspectReport.categories.map(item => <span className="pill neutral" key={item.category}>{item.category} {item.files}</span>)}</div>
                {backupInspectReport.credential_files.length ? <BackupDetails summary={`含凭据的文件 ${backupInspectReport.credential_files.length} 个`} items={backupInspectReport.credential_files} /> : null}
                {backupInspectReport.warnings.length ? <BackupDetails summary={`提醒 ${backupInspectReport.warnings.length} 条`} items={backupInspectReport.warnings} /> : null}
              </div>
            </section> : null}

            {backupRestoreReport ? <section className="card settings-section">
              <div className="card-header"><span>已恢复</span><Pill kind="success">{backupRestoreReport.restored.length} 个文件</Pill></div>
              <div className="software-update-summary">
                <div><span>凭据</span><strong>{backupRestoreReport.credentials} 项</strong></div>
                <div><span>恢复前快照</span><strong>{backupRestoreReport.snapshot}</strong></div>
              </div>
              <div className="card-body backup-body">
                <small>配置已写回本机，重启 HiMind Agent 后生效。</small>
                {backupRestoreReport.credential_failures.length ? <BackupDetails summary={`${backupRestoreReport.credential_failures.length} 项凭据需要重新配置`} items={backupRestoreReport.credential_failures} /> : null}
                {backupRestoreReport.missing_paths.length ? <BackupDetails summary={`${backupRestoreReport.missing_paths.length} 个路径在这台机器上不存在`} items={backupRestoreReport.missing_paths.map(item => `${item.source}：${item.path}`)} /> : null}
                {backupRestoreReport.pending_push.length ? <BackupDetails summary="需要重新推送" items={backupRestoreReport.pending_push} /> : null}
                {backupRestoreReport.warnings.length ? <BackupDetails summary={`提醒 ${backupRestoreReport.warnings.length} 条`} items={backupRestoreReport.warnings} /> : null}
              </div>
            </section> : null}

            <section className="card settings-section">
              <details className="backup-scope-details">
                <summary>备份范围</summary>
                {backupScope ? <div className="backup-scope">
                  <div>
                    <h3>进入备份包</h3>
                    <ul>{backupScope.entries.filter(item => item.included).map(item => <li key={item.name}><span>{item.name}</span><small>{item.category}</small></li>)}</ul>
                  </div>
                  <div>
                    <h3>不进备份包</h3>
                    <ul>{backupScope.entries.filter(item => !item.included).map(item => <li key={item.name}><span>{item.name}</span><small>{item.reason}</small></li>)}</ul>
                  </div>
                </div> : <div className="card-body"><span className="inline-feedback">正在读取备份范围…</span></div>}
              </details>
            </section>
          </> : null}

          {section === 'general' ? <>
            <section className="card settings-section">
              <div className="card-header">软件更新</div>
              <div className="software-update-summary">
                <div><span>当前版本</span><strong>v{updateStatus?.current_version || '—'}</strong></div>
                <div><span>最近检查</span><strong>{formatUpdateTime(updateStatus?.last_checked_at)}</strong></div>
              </div>
              <div className="card-body setting-list">
                <SettingRow title="自动检查更新"><label className="toggle"><input type="checkbox" checked={updateStatus?.auto_check ?? true} onChange={event => onUpdatePreferences(event.target.checked, event.target.checked && (updateStatus?.auto_download ?? true))} /><span className="slider"></span></label></SettingRow>
                <SettingRow title="自动下载更新" description="安装前会通知你"><label className="toggle"><input type="checkbox" disabled={!updateStatus?.auto_check} checked={updateStatus?.auto_download ?? true} onChange={event => onUpdatePreferences(updateStatus?.auto_check ?? true, event.target.checked)} /><span className="slider"></span></label></SettingRow>
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
                  <button className="btn" disabled={updateBusy || ['checking', 'downloading', 'installing'].includes(updateStatus?.status || '')} onClick={onCheckUpdate}>{updateBusy || updateStatus?.status === 'checking' ? <BusyIndicator size={15} /> : <RefreshCw size={15} />}检查更新</button>
                </div>
              </div>
            </section>
            <section className="card settings-section">
              <div className="card-header">启动设置</div>
              <div className="card-body setting-list">
                <SettingRow title="开机自启"><label className="toggle"><input type="checkbox" checked={settings.auto_start} onChange={event => onAutoStartChange(event.target.checked)} /><span className="slider"></span></label></SettingRow>
              </div>
            </section>
          </> : null}
        </div>
      {loginModalOpen ? <LoginModal configured={configured} username={loginUsername} password={loginPassword} onClose={onCloseLoginModal} onUsernameChange={onUsernameChange} onPasswordChange={onPasswordChange} onSave={onSaveLogin} onLogout={onLogoutLogin} onOpenInnerAdmin={onOpenInnerAdmin} /> : null}
      {svnModalOpen ? <SvnConnectionModal draft={svnDraft} exists={svnConnections.length > 0} onClose={onCloseSvnModal} onChange={onSvnDraftChange} onSave={onSaveSvnConnection} /> : null}
      {pendingApprovalProfile ? <ApprovalTrustConfirmation profile={pendingApprovalProfile} onClose={() => setPendingApprovalProfile(null)} onConfirm={(durationSeconds) => { onApprovalProfileChange(pendingApprovalProfile, true, durationSeconds); setPendingApprovalProfile(null); }} /> : null}
      {pendingRemoteRuntimeUnrestricted ? <RemoteRuntimeUnrestrictedConfirmation onClose={() => setPendingRemoteRuntimeUnrestricted(null)} onConfirm={() => { onRemoteExecutionChange(pendingRemoteRuntimeUnrestricted, true); setPendingRemoteRuntimeUnrestricted(null); }} /> : null}
      {pendingRuntimeUninstall ? <RuntimeUninstallConfirmation onClose={() => setPendingRuntimeUninstall(false)} onConfirm={() => { setPendingRuntimeUninstall(false); void startBuiltinAIRuntimeOperation('uninstall'); }} /> : null}
      {pendingBackupRestore ? <BackupRestoreConfirmation onClose={() => setPendingBackupRestore(false)} onConfirm={() => void restoreBackup()} /> : null}
    </>
  );
}

function runtimeActionLabel(operation: string) {
  if (operation === 'update') return '更新';
  if (operation === 'repair') return '修复';
  if (operation === 'local') return '本地安装';
  if (operation === 'uninstall') return '卸载';
  return '安装';
}

function presentRuntimeMessage(message?: string) {
  const value = (message || '').trim();
  if (!value) return '';
  return value
    .replace(/HiMind AI\s*运行时/g, 'HiMind AI')
    .replace(/本机\s*组件/g, 'HiMind AI')
    .replace(/运行时/g, 'HiMind AI')
    .replace(/组件/g, 'HiMind AI');
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

function BackupDetails({ summary, items }: { summary: string; items: string[] }) {
  return (
    <details className="backup-details">
      <summary>{summary}</summary>
      <ul>{items.map(item => <li key={item}>{item}</li>)}</ul>
    </details>
  );
}

function formatBackupBytes(bytes: number): string {
  if (!bytes || bytes < 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let value = bytes;
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index += 1;
  }
  return `${index === 0 || value >= 10 ? Math.round(value) : value.toFixed(1)} ${units[index]}`;
}

function formatBackupTime(value: string): string {
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? value || '—' : parsed.toLocaleString();
}

function BackupRestoreConfirmation({ onClose, onConfirm }: { onClose: () => void; onConfirm: () => void }) {
  return (
    <div className="modal-backdrop" onClick={onClose} role="presentation">
      <div className="modal" role="dialog" aria-modal="true" aria-labelledby="backup-restore-title" onClick={event => event.stopPropagation()}>
        <div className="modal-header"><div><h3 id="backup-restore-title">从备份包恢复？</h3><p>会覆盖本机账号、凭据与已装能力；恢复前自动快照，包外文件不受影响。</p></div><IconButton icon={X} label="关闭" onClick={onClose} /></div>
        <div className="modal-body">
          <div className="full-access-warning"><LockKeyhole size={20} /><div><strong>凭据按当前 Windows 账号重新加密</strong><span>包内凭据会用你填的口令改写成只对本机生效；口令不对时不覆盖任何文件。</span></div></div>
          <div className="modal-actions"><span /><div className="actions-row"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-danger" onClick={onConfirm}><RotateCcw size={15} />选择备份包并恢复</button></div></div>
        </div>
      </div>
    </div>
  );
}

function RuntimeUninstallConfirmation({ onClose, onConfirm }: { onClose: () => void; onConfirm: () => void }) {
  return (
    <div className="modal-backdrop" role="presentation">
      <div className="modal" role="dialog" aria-modal="true" aria-labelledby="runtime-uninstall-title">
        <div className="modal-header"><div><h3 id="runtime-uninstall-title">卸载 HiMind AI？</h3><p>HiMind AI 将暂时不可用，个人模型服务、技能、插件和用户数据会保留。</p></div><IconButton icon={X} label="关闭" onClick={onClose} /></div>
        <div className="modal-body"><div className="modal-actions"><span /><div className="actions-row"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-danger" onClick={onConfirm}><Trash2 size={15} />确认卸载</button></div></div></div>
      </div>
    </div>
  );
}

function RemoteRuntimeUnrestrictedConfirmation({ onClose, onConfirm }: { onClose: () => void; onConfirm: () => void }) {
  return (
    <div className="modal-backdrop" onClick={onClose} role="presentation">
      <div className="modal" role="dialog" aria-modal="true" aria-labelledby="remote-runtime-unrestricted-title" onClick={event => event.stopPropagation()}>
        <div className="modal-header"><div><h3 id="remote-runtime-unrestricted-title">允许远程任务访问全部文件？</h3><p>仅在任务确实需要访问项目目录以外的文件时开启。</p></div><IconButton icon={X} label="关闭" onClick={onClose} /></div>
        <div className="modal-body">
          <div className="full-access-warning"><ShieldAlert size={20} /><div><strong>远程任务将能访问当前 Windows 账户允许的本机资源</strong><span>可访问项目目录以外的文件和网络资源，仍受审批设置与 Windows 权限限制。</span></div></div>
          <div className="modal-actions"><span /><div className="actions-row"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-danger" onClick={onConfirm}><Bot size={15} />确认启用</button></div></div>
        </div>
      </div>
    </div>
  );
}

const TRUST_DURATION_OPTIONS: { value: number; label: string }[] = [
  { value: 3600, label: '1 小时' },
  { value: 10800, label: '3 小时' },
  { value: 86400, label: '1 天' },
  { value: 0, label: '永久（直到撤销）' },
];

function formatTrustRemaining(seconds?: number): string {
  if (!seconds || seconds <= 0) {
    return '0 分钟';
  }
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.ceil((seconds % 3600) / 60);
  if (hours > 0) {
    return minutes > 0 ? `${hours} 小时 ${minutes} 分` : `${hours} 小时`;
  }
  return `${minutes} 分钟`;
}

function ApprovalTrustConfirmation({ profile, onClose, onConfirm }: { profile: 'trusted' | 'full_access'; onClose: () => void; onConfirm: (durationSeconds: number) => void }) {
  const [durationSeconds, setDurationSeconds] = useState(3600);
  const durationLabel = TRUST_DURATION_OPTIONS.find(option => option.value === durationSeconds)?.label ?? '1 小时';
  const fullAccess = profile === 'full_access';
  const strategyLabel = fullAccess ? '完全放行' : '完全信任';
  return (
    <div className="modal-backdrop" onClick={onClose} role="presentation">
      <div className="modal approval-trust-modal" role="dialog" aria-modal="true" aria-labelledby="approval-trust-title" onClick={event => event.stopPropagation()}>
        <div className="modal-header approval-trust-header">
          <div className="approval-trust-heading"><div className="approval-trust-icon"><ShieldAlert size={20} /></div><div><h3 id="approval-trust-title">启用{strategyLabel}</h3><p>未来 {durationLabel} 内，{fullAccess ? '所有可执行受控操作' : '查询、修改和高风险操作'}可以自动执行。</p></div></div>
          <IconButton icon={X} label="关闭" onClick={onClose} />
        </div>
        <div className="modal-body approval-trust-body">
          <div className="approval-trust-intro"><strong>{fullAccess ? '受控操作将不再弹出确认' : '常规及高风险操作将自动执行'}</strong><span>授权仅适用于当前应用，可随时在审批中心或设置中恢复。</span></div>
          <div className="approval-trust-scope">
            <div className="approval-trust-scope-item"><FolderOpen size={17} /><div><strong>本地文件</strong><span>删除、批量清理或覆盖工作区文件。</span></div></div>
            <div className="approval-trust-scope-item"><Database size={17} /><div><strong>工作台数据</strong><span>删除项目或记录、解除关联、替换人员、发布变更。</span></div></div>
            <div className="approval-trust-scope-item"><Globe2 size={17} /><div><strong>第三方工具</strong><span>{fullAccess ? '所有受控的第三方操作。' : '高风险的外部写入和集成操作。'}</span></div></div>
          </div>
          <div className="approval-trust-boundaries"><LockKeyhole size={16} /><div><strong>策略边界</strong><span>{fullAccess ? '单项规则、系统保护目录和工作台权限仍然有效。' : '普通查询、修改和高风险操作将自动放行；最高风险操作仍会请求确认。'}</span></div></div>
          <div className="field-group approval-trust-duration">
            <label className="field-label" htmlFor="approval-trust-duration">授权有效期</label>
            <select id="approval-trust-duration" value={durationSeconds} onChange={event => setDurationSeconds(Number(event.target.value))}>
              {TRUST_DURATION_OPTIONS.map(option => <option key={option.value} value={option.value}>{option.label}</option>)}
            </select>
          </div>
          <div className="approval-trust-meta"><span><Clock3 size={14} />有效期 {durationLabel}</span><span><CheckCircle2 size={14} />可随时撤销</span></div>
          <div className="modal-actions"><span /><div className="actions-row"><button className="btn" onClick={onClose}>暂不启用</button><button className="btn btn-danger" onClick={() => onConfirm(durationSeconds)}><KeyRound size={15} />确认{strategyLabel} {durationLabel}</button></div></div>
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

/**
 * 设置项说明只写「非默认行为」和「后果」。
 * 默认值、一目了然的行为不写；`description` 省略时整行不渲染。
 */
function SettingRow({ title, description, children }: { title: string; description?: string; children: ReactNode }) {
  return (
    <div className="setting-row">
      <div><div className="label-text">{title}</div>{description ? <div className="label-desc">{description}</div> : null}</div>
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
  const resolve = (risk: 'R1' | 'R2' | 'R3' | 'R4'): 'manual' | 'auto_approve' | 'auto_deny' => {
    const rank = Number(risk.slice(1));
    for (const key of [`risk:${risk}`, 'controlled_operation', '*']) {
      const mode = rules[key];
      if (mode === 'auto_deny') return 'auto_deny';
      if (mode === 'manual') return 'manual';
      if (mode === 'auto_approve' && (rank < 3 || (rank === 3 && ['trusted', 'full_access'].includes(profile)) || (rank === 4 && profile === 'full_access'))) return 'auto_approve';
    }
    if (profile === 'silent_deny') return 'auto_deny';
    if (profile === 'full_access' && rank <= 4) return 'auto_approve';
    if (profile === 'trusted' && rank <= 3) return 'auto_approve';
    if (profile === 'relaxed' && rank <= 2) return 'auto_approve';
    if (profile === 'balanced' && rank <= 1) return 'auto_approve';
    return 'manual';
  };
  return { read: resolve('R1'), write: resolve('R2'), high_risk: resolve('R3'), system: resolve('R4') };
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
        <input id={`remote-client-${option.vendor}`} aria-label={`${option.name}路径`} value={path} onChange={event => onPathChange(event.target.value)} onBlur={() => { if (dirty && path.trim()) onSave(); }} placeholder={detectedPath || `选择 ${option.name}.exe`} title={path || detectedPath || ''} />
        <button type="button" className="btn btn-icon" title={`选择 ${option.name} 程序`} aria-label={`选择 ${option.name} 程序`} disabled={busy} onClick={onPick}>{busy ? <BusyIndicator size={16} /> : <FolderOpen size={16} />}</button>
      </div>
      {feedback || status?.configured_by === 'manual' ? (
        <div className="remote-client-footer">
          {feedback ? <span className="inline-feedback visible" role="status">{feedback}</span> : null}
          <div className="remote-client-actions">
            {status?.configured_by === 'manual' ? <button type="button" className="btn btn-danger-quiet" title="清除手动路径" aria-label={`清除 ${option.name} 手动路径`} disabled={busy} onClick={onClear}><Trash2 size={14} /></button> : null}
          </div>
        </div>
      ) : null}
    </div>
  );
}

function LoginModal({ configured, username, password, onClose, onUsernameChange, onPasswordChange, onSave, onLogout, onOpenInnerAdmin }: { configured: boolean; username: string; password: string; onClose: () => void; onUsernameChange: (value: string) => void; onPasswordChange: (value: string) => void; onSave: () => void; onLogout: () => void; onOpenInnerAdmin: () => void }) {
  return (
    <div className="modal-backdrop" onClick={onClose} role="presentation">
      <div className="modal" role="dialog" aria-modal="true" aria-labelledby="login-modal-title" onClick={event => event.stopPropagation()}>
        <div className="modal-header"><div><h3 id="login-modal-title">配置内网账号</h3><p>凭据仅保存在这台电脑。</p></div><IconButton icon={X} label="关闭" onClick={onClose} /></div>
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
