import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { Activity, ArrowUpRight, Blocks, CircleAlert, Download, LogIn, MessageCircle, MoreHorizontal, PlugZap, RefreshCw, Settings } from 'lucide-react';
import { agentApi, type BuiltinAIRuntimeActivity, type BuiltinAIRuntimeInstallationStatus, type BuiltinAiWorkspaceTarget, type DashboardAuthorizationProgress, type DashboardIdentityStatus } from '../services/agentApi';
import { errorDetail } from '../types';
import { ActionMenu, ActionMenuItem } from '../components/ActionMenu';
import { BuiltinAiExtensionsDialog } from '../components/BuiltinAiExtensionsDialog';
import { BusyIndicator } from '../components/BusyIndicator';

type BuiltinAiPageProps = {
  independentMode: boolean;
  identity: DashboardIdentityStatus | null;
  authorization: DashboardAuthorizationProgress | null;
  authorizationBusy: boolean;
  onStartAuthorization: () => void;
  onCancelAuthorization: () => void;
  onOpenAuthorization: () => void;
  onOpenSettings: () => void;
  onOpenAiConnections: () => void;
  onOpenCapabilities: () => void;
  /** 挑 MCP 工具的地方在市场；会话旁只留一个入口跳过去。 */
  onOpenMcpTools: () => void;
  onToolContextChanged: () => void;
  skillCount: number | null;
  workspaceTarget: BuiltinAiWorkspaceTarget;
  workspaceRequestRevision: number;
};

export function BuiltinAiPage({
  independentMode: independentModeFromStatus,
  identity,
  authorization,
  authorizationBusy,
  onStartAuthorization,
  onCancelAuthorization,
  onOpenAuthorization,
  onOpenSettings,
  onOpenAiConnections,
  onOpenCapabilities,
  onOpenMcpTools,
  onToolContextChanged,
  skillCount,
  workspaceTarget,
  workspaceRequestRevision,
}: BuiltinAiPageProps) {
  const [sessionUrl, setSessionUrl] = useState('');
  const [connecting, setConnecting] = useState(false);
  const [browserOpening, setBrowserOpening] = useState(false);
  const [frameLoaded, setFrameLoaded] = useState(false);
  // 会话重开时即使 URL 一个字没变，也要换一个 iframe 元素：同一个元素重新指向
  // 同一个地址，浏览器不会再发 load，加载遮罩就会一直挂在那儿（会话其实已经开了）。
  const [frameKey, setFrameKey] = useState(0);
  const [frameStalled, setFrameStalled] = useState(false);
  const frameRetry = useRef(0);
  const [connectionError, setConnectionError] = useState('');
  const [sessionNotice, setSessionNotice] = useState('');
  const [extensionsOpen, setExtensionsOpen] = useState(false);
  const [syncingModels, setSyncingModels] = useState(false);
  const [modelSyncMessage, setModelSyncMessage] = useState('');
  const [runtimeInstallation, setRuntimeInstallation] = useState<BuiltinAIRuntimeInstallationStatus | null>(null);
  const [activityOpen, setActivityOpen] = useState(false);
  const [runtimeSessions, setRuntimeSessions] = useState<BuiltinAIRuntimeActivity[]>([]);
  const [activityError, setActivityError] = useState('');
  const handledWorkspaceRequest = useRef(0);
  const probedWorkspaceTarget = useRef('');
  const independentMode = independentModeFromStatus;
  const authorizationActive = authorization?.state === 'starting' || authorization?.state === 'pending';
  const runtimeReady = runtimeInstallation?.runtime.status === 'ready' && runtimeInstallation.runtime.compatible;
  // 会话不再以“登录工作台”为前提：工作台账号、本机自定义服务、运行时自带
  // 的 Provider 都可以支撑一次会话，能不能用交给启动结果来说。
  const canStartSession = runtimeReady;

  const refreshActivity = useCallback(async () => {
    if (!identity?.authorized || independentMode) return;
    try {
      const result = await agentApi.builtinAiActivity();
      setRuntimeSessions(result.items || []);
      setActivityError('');
    } catch (error) {
      setActivityError(errorDetail(error));
    }
  }, [identity?.authorized, independentMode]);

  const refreshRuntimeInstallation = useCallback(async () => {
    try {
      setRuntimeInstallation(await agentApi.builtinAiRuntimeInstallationStatus());
    } catch (error) {
      setRuntimeInstallation(current => current || {
        state: 'failed',
        operation: 'none',
        stage: 'failed',
        progress_percent: 0,
        message: '无法检查 HiMind AI',
        error: errorDetail(error),
        runtime: { provider: 'himind.builtin', status: 'unavailable', version: '', compatible: false, message: '', diagnostics: { engine_id: '', executable_path: '', contract_version: 1, update_mode: '' } },
        update_available: false,
        available_version: '',
        release_notes: '',
        mandatory_update: false,
      });
    }
  }, []);

  const installRuntime = useCallback(async () => {
    try {
      setRuntimeInstallation(await agentApi.startBuiltinAiRuntimeInstall());
    } catch (error) {
      setRuntimeInstallation(current => current ? { ...current, state: 'failed', stage: 'failed', message: 'HiMind AI 安装失败', error: errorDetail(error) } : current);
    }
  }, []);

  useEffect(() => {
    void refreshRuntimeInstallation();
  }, [refreshRuntimeInstallation]);

  // 只有面板真的在屏幕上了才轮询：以前是登录后就不停地拉，面板关着也照样每 8 秒
  // 打一次运行时接口，纯属白花钱。
  useEffect(() => {
    if (!activityOpen || !identity?.authorized || independentMode) return;
    void refreshActivity();
    const timer = window.setInterval(() => void refreshActivity(), 8000);
    return () => window.clearInterval(timer);
  }, [activityOpen, identity?.authorized, independentMode, refreshActivity]);

  useEffect(() => {
    if (runtimeInstallation?.state !== 'working') return;
    const timer = window.setInterval(() => { void refreshRuntimeInstallation(); }, 700);
    return () => window.clearInterval(timer);
  }, [refreshRuntimeInstallation, runtimeInstallation?.state]);

  const connect = useCallback(async () => {
    if (connecting) return;
    setConnecting(true);
    setConnectionError('');
    frameRetry.current = 0;
    setFrameStalled(false);
    setFrameLoaded(false);
    try {
      const request = builtinAiSessionTarget(workspaceTarget);
      const url = await agentApi.startBuiltinAiSession(request);
      setFrameKey(key => key + 1);
      setSessionUrl(url);
      // 会话能启动、但模型凭据不是来自工作台时，把真实原因放在会话上方，
      // 而不是让用户自己去猜为什么消息发不出去。
      setSessionNotice((await agentApi.builtinAiSessionNotice(builtinAiWorkspaceRoot(workspaceTarget)).catch(() => null)) || '');
    } catch (error) {
      setSessionUrl('');
      setSessionNotice('');
      setConnectionError(presentConnectionError(error));
    } finally {
      setConnecting(false);
    }
  }, [connecting, workspaceTarget]);

  const openInBrowser = useCallback(async () => {
    if (browserOpening || connecting || !runtimeReady || !canStartSession) return;
    setBrowserOpening(true);
    setConnectionError('');
    try {
      const url = await agentApi.openBuiltinAiWeb(builtinAiSessionTarget(workspaceTarget));
      if (url && url !== sessionUrl) {
        setSessionUrl(url);
        frameRetry.current = 0;
        setFrameStalled(false);
        setFrameLoaded(false);
        setFrameKey(key => key + 1);
      }
    } catch (error) {
      setConnectionError(presentConnectionError(error));
    } finally {
      setBrowserOpening(false);
    }
  }, [browserOpening, canStartSession, connecting, runtimeReady, sessionUrl, workspaceTarget]);

  useEffect(() => {
    if (!workspaceRequestRevision || connecting || handledWorkspaceRequest.current === workspaceRequestRevision) return;
    handledWorkspaceRequest.current = workspaceRequestRevision;
    setSessionUrl('');
    setConnectionError('');
    void connect();
  }, [connect, connecting, workspaceRequestRevision]);

  // 会话按工作区复用：页面重新挂载或切到已有会话的工作区时，先找回正在跑的
  // 那条会话，避免重复走一次"正在启动"。
  const workspaceTargetKey = `${workspaceTarget?.kind ?? ''}|${workspaceTarget?.path ?? ''}|${workspaceTarget?.kind === 'project' ? workspaceTarget.projectId : ''}`;
  useEffect(() => {
    if (probedWorkspaceTarget.current === workspaceTargetKey) return;
    probedWorkspaceTarget.current = workspaceTargetKey;
    const root = builtinAiWorkspaceRoot(workspaceTarget);
    if (!root) return;
    const normalized = normalizeWorkspacePath(root);
    let cancelled = false;
    void agentApi.listBuiltinAiSessions().then((sessions) => {
      if (cancelled) return;
      const existing = sessions.find((item) => normalizeWorkspacePath(item.workspace_root) === normalized);
      if (!existing) return;
      setSessionUrl(existing.url);
      setSessionNotice(existing.notice || '');
    }).catch(() => undefined);
    return () => { cancelled = true; };
  }, [workspaceTarget, workspaceTargetKey]);

  useEffect(() => {
    if (!canStartSession || !runtimeReady || sessionUrl || connecting || connectionError) return;
    void connect();
  }, [canStartSession, connect, connecting, connectionError, runtimeReady, sessionUrl]);

  // 加载态兜底：会话页长时间没交出 load 事件时，先自己重挂一次；再不行就把
  // 「重试 / 在浏览器打开」交到用户手上，而不是让一个转圈把整块会话区锁死。
  useEffect(() => {
    if (!sessionUrl || frameLoaded) {
      setFrameStalled(false);
      return;
    }
    setFrameStalled(false);
    const timer = window.setTimeout(() => setFrameStalled(true), 12000);
    return () => window.clearTimeout(timer);
  }, [frameKey, frameLoaded, sessionUrl]);

  useEffect(() => {
    if (!frameStalled || frameRetry.current >= 2) return;
    frameRetry.current += 1;
    setFrameLoaded(false);
    setFrameKey(key => key + 1);
  }, [frameStalled]);

  const syncModels = useCallback(async () => {
    if (!sessionUrl || syncingModels) return;
    setSyncingModels(true);
    setModelSyncMessage('');
    try {
      const result = await agentApi.syncBuiltinAiModels();
      if (result.session_url && result.session_url !== sessionUrl) {
        setSessionUrl(await agentApi.startBuiltinAiSession(builtinAiSessionTarget(workspaceTarget)));
        frameRetry.current = 0;
        setFrameStalled(false);
        setFrameLoaded(false);
        setFrameKey(key => key + 1);
      }
      setModelSyncMessage(result.status === 'updated' || result.status === 'restarted'
        ? `已同步 ${result.model_count} 个模型`
        : '模型已是最新');
    } catch (error) {
      setModelSyncMessage('同步失败，请稍后重试');
    } finally {
      setSyncingModels(false);
    }
  }, [sessionUrl, syncingModels, workspaceTarget]);

  return (
    <section className="builtin-ai-page" aria-label="HiMind AI">
      <header className="builtin-ai-toolbar">
        <div className="builtin-ai-title">
          <span className="builtin-ai-mark"><MessageCircle size={17} /></span>
          {/* 扩展工作区不是一个"项目"，它只是一个开发目录，文案必须跟着目标类型走。 */}
          <div><h2>HiMind AI</h2><span>{workspaceTarget ? `${workspaceTarget.kind === 'extension-workspace' ? '当前工作区' : '当前项目'}：${workspaceTarget.name}` : 'AI 助手'}</span></div>
        </div>
        <div className="builtin-ai-toolbar-actions">
          {/* 对话页只保留一个主入口：给 HiMind AI 加工具。打开网页版、同步模型、
              看活动、改模型服务都是次要动作，收进「更多」，避免标题栏变成按钮墙。 */}
          <button type="button" className={`builtin-ai-tools-button ${extensionsOpen ? 'active' : ''}`} onClick={() => setExtensionsOpen(true)} title="让 HiMind AI 在对话里使用本机工具" aria-label="AI 工具"><Blocks size={15} aria-hidden="true" />AI 工具</button>
          <ActionMenu className="builtin-ai-more" icon={<MoreHorizontal size={15} aria-hidden="true" />} title="更多">
            {close => <>
              <ActionMenuItem icon={<ArrowUpRight size={16} aria-hidden="true" />} label="在浏览器打开" title="在系统浏览器中打开 HiMind AI" disabled={!runtimeReady || !canStartSession || connecting || browserOpening} onClick={() => { close(); void openInBrowser(); }} />
              {!independentMode ? <ActionMenuItem icon={<RefreshCw size={16} aria-hidden="true" />} label="同步模型" title="把工作台可用的模型同步到 HiMind AI" state={syncingModels ? '同步中…' : modelSyncMessage || undefined} disabled={!sessionUrl || syncingModels} onClick={() => { close(); void syncModels(); }} /> : null}
              {!independentMode ? <ActionMenuItem icon={<Activity size={16} aria-hidden="true" />} label="活动" title="查看 HiMind AI 的协同活动" state={activityOpen ? '已展开' : undefined} onClick={() => { close(); setActivityOpen(open => !open); void refreshActivity(); }} /> : null}
              <div className="app-menu-separator" role="separator" />
              <ActionMenuItem icon={<Settings size={16} aria-hidden="true" />} label="模型服务" title="管理模型凭据与运行环境" onClick={() => { close(); onOpenAiConnections(); }} />
            </>}
          </ActionMenu>
          {modelSyncMessage ? <span className="builtin-ai-sync-message" role="status">{modelSyncMessage}</span> : null}
        </div>
      </header>

      <div className="builtin-ai-workspace">
        {sessionUrl ? (
          <>
            {sessionNotice ? (
              <div className="builtin-ai-notice" role="status">
                <CircleAlert size={15} />
                <span>{sessionNotice}</span>
                {/* 提示里点名了模型服务时，就近给一个入口，别让用户自己找。
                    旧版文案（「设置 → AI 服务」）可能还留在已保存的会话里，一并认出来。 */}
                {sessionNotice.includes('模型服务') || sessionNotice.includes('设置 → AI 服务') ? (
                  <button type="button" className="builtin-ai-notice-action" title="管理模型凭据与运行环境" onClick={onOpenAiConnections}>
                    <Settings size={14} />模型服务
                  </button>
                ) : null}
                {/* 这条提示讲的是本机模型凭据，别把「登录工作台」混进来：工作台
                    账号在左下角账号卡片里管，会话不连工作台也照常可用。 */}
                <button type="button" className="builtin-ai-notice-action" title="按当前配置重启这条会话" onClick={() => void connect()} disabled={connecting}>
                  <RefreshCw size={14} />重启会话
                </button>
                <button type="button" className="builtin-ai-notice-close" title="关闭提示" aria-label="关闭提示" onClick={() => setSessionNotice('')}>✕</button>
              </div>
            ) : null}
            <div className="builtin-ai-session-shell">
              {/* 加载态盖住会话区就够：它是绝对定位的整块遮罩，放在工作区这一层
                  会把上方的提示条一起压住，会话每次重开时提示就闪一下不见了。 */}
              {!frameLoaded ? (
                frameStalled ? (
                  <WorkspaceStatus
                    icon={<CircleAlert size={21} />}
                    title="会话还没有打开"
                    description="加载时间偏长，可以重试一次，或改用系统浏览器打开。"
                    actions={<>
                      <button type="button" className="btn btn-primary" onClick={() => { frameRetry.current = 0; setFrameLoaded(false); setFrameKey(key => key + 1); }}><RefreshCw size={15} />重试</button>
                      <button type="button" className="btn" disabled={browserOpening} onClick={() => void openInBrowser()}><ArrowUpRight size={15} />在浏览器打开</button>
                    </>}
                  />
                ) : (
                  <WorkspaceStatus icon={<BusyIndicator size={21} />} title="正在打开会话" description="正在加载 HiMind AI 工作台" />
                )
              ) : null}
              <iframe
                key={frameKey}
                className={frameLoaded ? 'loaded' : ''}
                title="HiMind AI 会话"
                src={sessionUrl}
                onLoad={() => setFrameLoaded(true)}
              />
              {activityOpen ? <RuntimeActivityPanel sessions={runtimeSessions} error={activityError} onRefresh={() => void refreshActivity()} /> : null}
            </div>
          </>
        ) : !independentMode && authorizationActive ? (
          <WorkspaceStatus
            icon={<LogIn size={22} />}
            title={authorization?.state === 'starting' ? '正在打开登录页面' : '请在浏览器中确认登录'}
            description={authorization?.user_code ? `确认码 ${authorization.user_code}` : '确认后会自动返回并连接 HiMind AI'}
            actions={<>
              {authorization?.verification_uri_complete ? <button type="button" className="btn btn-primary" onClick={onOpenAuthorization}><ArrowUpRight size={15} />打开确认页面</button> : null}
              <button type="button" className="btn" onClick={onCancelAuthorization}>取消</button>
            </>}
          />
        ) : identity === null && !independentMode ? (
          <WorkspaceStatus icon={<BusyIndicator size={21} />} title="正在准备 HiMind AI" description="正在检查账号状态" />
        ) : !runtimeInstallation ? (
          <WorkspaceStatus icon={<BusyIndicator size={21} />} title="正在检查 HiMind AI" description="正在确认 HiMind AI 是否可用。" />
        ) : runtimeInstallation.state === 'working' ? (
          <RuntimeInstallationStatus installation={runtimeInstallation} />
        ) : !runtimeReady ? (
          <WorkspaceStatus
            tone={runtimeInstallation.state === 'failed' ? 'error' : 'default'}
            icon={runtimeInstallation.state === 'failed' ? <CircleAlert size={22} /> : <Download size={22} />}
            title="安装 HiMind AI"
            description={runtimeInstallation.state === 'failed' ? (runtimeInstallation.error || '安装没有完成，请重试。') : '首次使用需要安装 HiMind AI。'}
            actions={<>
              <button type="button" className="btn btn-primary" onClick={() => void installRuntime()}><Download size={15} />{runtimeInstallation.state === 'failed' ? '重试安装' : '安装 HiMind AI'}</button>
              <button type="button" className="btn" onClick={onOpenSettings}><Settings size={15} />打开设置</button>
            </>}
          />
        ) : connecting ? (
          <WorkspaceStatus icon={<BusyIndicator size={21} />} title="正在准备会话" description="首次启动可能需要一点时间" />
        ) : connectionError ? (
          <WorkspaceStatus
            tone="error"
            icon={<CircleAlert size={22} />}
            title="会话没有启动"
            description={connectionError}
            actions={<>
              <button type="button" className="btn btn-primary" onClick={() => void connect()}><PlugZap size={15} />重新连接</button>
              {!identity?.authorized && !independentMode ? <button type="button" className="btn" disabled={authorizationBusy} onClick={onStartAuthorization}><LogIn size={15} />登录 HiMind</button> : null}
              <button type="button" className="btn" onClick={onOpenAiConnections}><Settings size={15} />配置本机模型服务</button>
              <button type="button" className="btn" onClick={() => void openInBrowser()}><ArrowUpRight size={15} />打开网页版</button>
            </>}
          />
        ) : (
          <WorkspaceStatus icon={<BusyIndicator size={21} />} title="正在准备 HiMind AI" description="正在连接服务" />
        )}
      </div>
      <BuiltinAiExtensionsDialog
        open={extensionsOpen}
        skillCount={skillCount}
        onClose={() => setExtensionsOpen(false)}
        onRuntimeChanged={() => {
          setSessionUrl('');
          setFrameLoaded(false);
          setConnectionError('');
        }}
        onToolContextChanged={onToolContextChanged}
        onOpenMcpTools={onOpenMcpTools}
        onOpenCapabilities={() => { setExtensionsOpen(false); onOpenCapabilities(); }}
      />
    </section>
  );
}

function RuntimeActivityPanel({ sessions, error, onRefresh }: { sessions: BuiltinAIRuntimeActivity[]; error: string; onRefresh: () => void }) {
  return <aside className="builtin-ai-activity-panel" aria-label="协同活动">
    <header><div><strong>会话活动</strong><span>查看各入口的会话状态</span></div><button type="button" className="btn btn-icon" title="刷新活动" aria-label="刷新活动" onClick={onRefresh}><RefreshCw size={14} /></button></header>
    {error ? <div className="builtin-ai-activity-message error" role="alert">{error}</div> : null}
    {!error && !sessions.length ? <div className="builtin-ai-activity-message">暂无活动会话</div> : null}
    <div className="builtin-ai-activity-list">{sessions.map(activity => <div className="builtin-ai-activity-item" key={activity.session.id}>
      <div className="builtin-ai-activity-item-head"><span className={`builtin-ai-activity-dot ${activity.session.status}`} /><strong>{activity.conversation?.title || '未命名会话'}</strong><span>{runtimeSessionStatusLabel(activity.session.status)}</span></div>
      <small>{activity.conversation ? `入口：${activity.endpoints?.map(endpoint => endpoint.channel).join('、') || '本机'}` : '等待新消息'} · {formatActivityTime(activity.session.last_heartbeat_at)}</small>
      {activity.latest_turn ? <p className="builtin-ai-activity-preview">{activity.latest_turn.content}</p> : null}
    </div>)}</div>
  </aside>;
}

function runtimeSessionStatusLabel(status: string) {
  if (status === 'online') return '在线';
  if (status === 'idle') return '空闲';
  if (status === 'degraded') return '降级';
  if (status === 'offline') return '离线';
  if (status === 'revoked') return '已撤销';
  return status;
}

function formatActivityTime(value: string) {
  if (!value) return '刚刚';
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

function WorkspaceStatus({ icon, title, description, actions, tone = 'default' }: { icon: ReactNode; title: string; description: string; actions?: ReactNode; tone?: 'default' | 'error' }) {
  return <div className={`builtin-ai-state ${tone}`} role={tone === 'error' ? 'alert' : 'status'}><span className="builtin-ai-state-icon">{icon}</span><h3>{title}</h3><p>{description}</p>{actions ? <div className="builtin-ai-state-actions">{actions}</div> : null}</div>;
}

function RuntimeInstallationStatus({ installation }: { installation: BuiltinAIRuntimeInstallationStatus }) {
  const action = runtimeActionLabel(installation.operation);
  return <div className="builtin-ai-state" role="status">
    <span className="builtin-ai-state-icon"><BusyIndicator size={21} /></span>
    <h3>{presentRuntimeMessage(installation.message) || `正在${action} HiMind AI`}</h3>
    <p>{installation.operation === 'uninstall' ? '卸载期间 HiMind AI 暂不可用。' : '完成后会自动进入 HiMind AI。'}</p>
    <div className="builtin-ai-runtime-progress" aria-label={`${action}进度 ${installation.progress_percent}%`}>
      <div className="builtin-ai-runtime-progress-head"><span>{runtimeStageLabel(installation.stage)}</span><strong>{installation.progress_percent}%</strong></div>
      <div className="builtin-ai-runtime-progress-track"><span style={{ width: `${installation.progress_percent}%` }} /></div>
    </div>
  </div>;
}

function runtimeStageLabel(stage: string) {
  if (stage === 'resolving') return '检查安装包';
  if (stage === 'downloading') return '下载';
  if (stage === 'verifying') return '校验';
  if (stage === 'installing') return '安装';
  if (stage === 'uninstalling') return '卸载';
  return '正在准备';
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

function runtimeActionLabel(operation: string) {
  if (operation === 'update') return '更新';
  if (operation === 'repair') return '修复';
  if (operation === 'uninstall') return '卸载';
  return '安装';
}

function presentConnectionError(error: unknown) {
  const detail = errorDetail(error);
  const normalized = detail.toLowerCase();
  if (normalized.includes('dsh web authentication required')
    || normalized.includes('reopen the url printed by dsh web')) {
    return '网页会话认证已失效，请使用“打开网页版”重新建立会话。';
  }
  if (normalized.includes('independent mode')
    || normalized.includes('dsh 原生')
    || normalized.includes('settings.yaml')
    || (normalized.includes('provider') && normalized.includes('原生服务配置'))) {
    return '本机模型服务尚未配置，请在设置中完成配置后重试。';
  }
  if (normalized.includes('登录 himind')) return '当前登录状态已失效，请重新登录。';
  // 本机服务缺字段是最常见的自建服务故障，先于通用的「ai 服务」兜底认出来，
  // 否则会被误导成「账号没有分配服务」去找管理员。
  if (normalized.includes('模型或 base url')) return '本机模型服务的模型或 Base URL 不完整，请到「设置 → AI 连接 → 模型服务」补全。';
  // 后端同类错误改叫「模型服务」后也该命中这一条兜底，但不能按「模型服务」泛匹配，
  // 否则会把下面的「dsh 不可用」等更具体的本机故障也误判成账号没分配服务。
  if (normalized.includes('ai 服务') || normalized.includes('暂未分配可用模型服务')) {
    return '当前账号暂未分配可用的模型服务，请联系管理员。';
  }
  if (normalized.includes('运行时') && normalized.includes('修复')) return 'HiMind AI 需要修复，请在设置中处理。';
  if ((normalized.includes('运行时') || normalized.includes('组件')) && normalized.includes('安装')) return '请先安装 HiMind AI，再开始对话。';
  if (normalized.includes('项目工作区') || normalized.includes('workspace')) return '当前项目目录不可用，请重新选择后再试。';
  if (normalized.includes('dsh')) return '本机模型服务暂时不可用，请在设置中检查配置。';
  if (normalized.includes('正在启动')) return '会话仍在准备中，请稍后重新连接。';
  return '服务暂时不可用，请稍后重试。';
}

function builtinAiSessionTarget(workspaceTarget: BuiltinAiWorkspaceTarget) {
  if (workspaceTarget?.kind === 'project') return { projectId: workspaceTarget.projectId };
  // 会话按目录隔离：这里必须带上真实目录，否则多个扩展工作区会共用同一条会话。
  if (workspaceTarget?.kind === 'extension-workspace') return { workspaceRoot: workspaceTarget.path };
  return undefined;
}

/** 扩展工作区的会话按目录复用，用于在页面重新挂载时找回已经跑着的会话。 */
function builtinAiWorkspaceRoot(workspaceTarget: BuiltinAiWorkspaceTarget) {
  return workspaceTarget?.kind === 'extension-workspace' ? workspaceTarget.path : undefined;
}

/** 工作台回传的目录可能带大小写和分隔符差异，比较前先归一。 */
function normalizeWorkspacePath(path?: string | null) {
  return (path ?? '').trim().replace(/\//g, '\\').replace(/\\+$/, '').toLowerCase();
}
