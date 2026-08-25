import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { Activity, ArrowUpRight, Blocks, CircleAlert, Download, LoaderCircle, LogIn, MessageCircle, RefreshCw, Settings } from 'lucide-react';
import { agentApi, type BuiltinAIRuntimeActivity, type BuiltinAIRuntimeInstallationStatus, type BuiltinAIToolContextSummary, type BuiltinAiWorkspaceTarget, type DashboardAuthorizationProgress, type DashboardIdentityStatus } from '../services/agentApi';
import { errorDetail } from '../types';
import { BuiltinAiExtensionsDialog } from '../components/BuiltinAiExtensionsDialog';

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
  onOpenPlugins: () => void;
  onOpenSkills: () => void;
  onToolContextChanged: () => void;
  toolSummary: BuiltinAIToolContextSummary;
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
  onOpenPlugins,
  onOpenSkills,
  onToolContextChanged,
  toolSummary,
  workspaceTarget,
  workspaceRequestRevision,
}: BuiltinAiPageProps) {
  const [sessionUrl, setSessionUrl] = useState('');
  const [connecting, setConnecting] = useState(false);
  const [frameLoaded, setFrameLoaded] = useState(false);
  const [connectionError, setConnectionError] = useState('');
  const [extensionsOpen, setExtensionsOpen] = useState(false);
  const [syncingModels, setSyncingModels] = useState(false);
  const [modelSyncMessage, setModelSyncMessage] = useState('');
  const [runtimeInstallation, setRuntimeInstallation] = useState<BuiltinAIRuntimeInstallationStatus | null>(null);
  const [activityOpen, setActivityOpen] = useState(false);
  const [runtimeSessions, setRuntimeSessions] = useState<BuiltinAIRuntimeActivity[]>([]);
  const [activityError, setActivityError] = useState('');
  const handledWorkspaceRequest = useRef(0);
  const independentMode = independentModeFromStatus || identity?.state === 'independent';
  const canStartSession = Boolean(identity?.authorized || independentMode);
  const authorizationActive = authorization?.state === 'starting' || authorization?.state === 'pending';
  const runtimeReady = runtimeInstallation?.runtime.status === 'ready' && runtimeInstallation.runtime.compatible;

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
        message: '无法检查 HiMind AI 运行时',
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
      setRuntimeInstallation(current => current ? { ...current, state: 'failed', stage: 'failed', message: 'HiMind AI 运行时安装失败', error: errorDetail(error) } : current);
    }
  }, []);

  useEffect(() => {
    void refreshRuntimeInstallation();
  }, [refreshRuntimeInstallation]);

  useEffect(() => {
    if (!identity?.authorized) return;
    void refreshActivity();
    const timer = window.setInterval(() => void refreshActivity(), 8000);
    return () => window.clearInterval(timer);
  }, [identity?.authorized, refreshActivity]);

  useEffect(() => {
    if (runtimeInstallation?.state !== 'working') return;
    const timer = window.setInterval(() => { void refreshRuntimeInstallation(); }, 700);
    return () => window.clearInterval(timer);
  }, [refreshRuntimeInstallation, runtimeInstallation?.state]);

  const connect = useCallback(async () => {
    if (connecting) return;
    setConnecting(true);
    setConnectionError('');
    setFrameLoaded(false);
    try {
      const request = workspaceTarget?.kind === 'project'
        ? { projectId: workspaceTarget.projectId }
        : workspaceTarget?.kind === 'extension-workspace'
          ? { extensionWorkspace: true }
          : undefined;
      setSessionUrl(await agentApi.startBuiltinAiSession(request));
    } catch (error) {
      setSessionUrl('');
      setConnectionError(presentConnectionError(error));
    } finally {
      setConnecting(false);
    }
  }, [connecting, workspaceTarget]);

  useEffect(() => {
    if (!workspaceRequestRevision || connecting || handledWorkspaceRequest.current === workspaceRequestRevision) return;
    handledWorkspaceRequest.current = workspaceRequestRevision;
    setSessionUrl('');
    setConnectionError('');
    setFrameLoaded(false);
    void connect();
  }, [connect, connecting, workspaceRequestRevision]);

  useEffect(() => {
    if (!canStartSession || !runtimeReady || sessionUrl || connecting || connectionError) return;
    void connect();
  }, [canStartSession, connect, connecting, connectionError, runtimeReady, sessionUrl]);

  const syncModels = useCallback(async () => {
    if (!sessionUrl || syncingModels) return;
    setSyncingModels(true);
    setModelSyncMessage('');
    try {
      const result = await agentApi.syncBuiltinAiModels();
      if (result.session_url && result.session_url !== sessionUrl) {
        setSessionUrl(result.session_url);
        setFrameLoaded(false);
      }
      setModelSyncMessage(result.status === 'updated' || result.status === 'restarted'
        ? `已同步 ${result.model_count} 个模型`
        : '模型已是最新');
    } catch (error) {
      setModelSyncMessage('同步失败，请稍后重试');
    } finally {
      setSyncingModels(false);
    }
  }, [sessionUrl, syncingModels]);

  return (
    <section className="builtin-ai-page" aria-label="HiMind AI">
      <header className="builtin-ai-toolbar">
        <div className="builtin-ai-title">
          <span className="builtin-ai-mark"><MessageCircle size={17} /></span>
          <div><h2>HiMind AI</h2><span>{workspaceTarget ? `正在开发：${workspaceTarget.name}` : '智能工作助手'}</span></div>
        </div>
        <div className="builtin-ai-toolbar-actions">
          {!independentMode ? <button type="button" className="builtin-ai-tools-button" onClick={() => void syncModels()} disabled={!sessionUrl || syncingModels} title="同步可用模型">
            <RefreshCw className={syncingModels ? 'spin' : ''} size={15} />{syncingModels ? '同步中' : '同步模型'}
          </button> : null}
          <button type="button" className="builtin-ai-tools-button" onClick={onOpenAiConnections} title="管理 AI 服务"><Settings size={15} />AI 服务</button>
          <button type="button" className={`builtin-ai-tools-button ${extensionsOpen ? 'active' : ''}`} onClick={() => setExtensionsOpen(true)} title="管理扩展"><Blocks size={15} />扩展</button>
          {!independentMode ? <button type="button" className={`builtin-ai-tools-button ${activityOpen ? 'active' : ''}`} onClick={() => { setActivityOpen(open => !open); void refreshActivity(); }} title="查看协同活动"><Activity size={15} />活动</button> : null}
          {modelSyncMessage ? <span className="builtin-ai-sync-message" role="status">{modelSyncMessage}</span> : null}
        </div>
      </header>

      <div className="builtin-ai-workspace">
        {sessionUrl ? (
          <>
            {!frameLoaded ? <WorkspaceStatus icon={<LoaderCircle className="spin" size={21} />} title="正在打开会话" description="马上就好" /> : null}
            <div className="builtin-ai-session-shell">
              <iframe
                className={frameLoaded ? 'loaded' : ''}
                title="HiMind AI 会话"
                src={sessionUrl}
                onLoad={() => setFrameLoaded(true)}
                referrerPolicy="no-referrer"
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
          <WorkspaceStatus icon={<LoaderCircle className="spin" size={21} />} title="正在准备 HiMind AI" description="正在检查账号状态" />
        ) : identity && !identity.authorized && !independentMode ? (
          <WorkspaceStatus
            icon={<LogIn size={22} />}
            title="登录后开始对话"
            description="使用组织提供的 AI 服务，无需单独配置账号或密钥"
            actions={<button type="button" className="btn btn-primary" disabled={authorizationBusy} onClick={onStartAuthorization}>{authorizationBusy ? <LoaderCircle className="spin" size={15} /> : <LogIn size={15} />}{authorizationBusy ? '正在连接' : '登录 HiMind'}</button>}
          />
        ) : !runtimeInstallation ? (
          <WorkspaceStatus icon={<LoaderCircle className="spin" size={21} />} title="正在检查 HiMind AI 运行时" description="正在确认本机是否已准备好 HiMind AI。" />
        ) : runtimeInstallation.state === 'working' ? (
          <RuntimeInstallationStatus installation={runtimeInstallation} />
        ) : !runtimeReady ? (
          <WorkspaceStatus
            tone={runtimeInstallation.state === 'failed' ? 'error' : 'default'}
            icon={runtimeInstallation.state === 'failed' ? <CircleAlert size={22} /> : <Download size={22} />}
            title="安装 HiMind AI 运行时"
            description={runtimeInstallation.state === 'failed' ? (runtimeInstallation.error || '安装没有完成，请重试。') : '首次使用 HiMind AI 需要安装运行时，安装完成后即可开始对话。'}
            actions={<>
              <button type="button" className="btn btn-primary" onClick={() => void installRuntime()}><Download size={15} />{runtimeInstallation.state === 'failed' ? '重试安装' : '安装运行时'}</button>
              <button type="button" className="btn" onClick={onOpenSettings}><Settings size={15} />打开设置</button>
            </>}
          />
        ) : connecting ? (
          <WorkspaceStatus icon={<LoaderCircle className="spin" size={21} />} title="正在准备会话" description="首次启动可能需要一点时间" />
        ) : connectionError ? (
          <WorkspaceStatus
            tone="error"
            icon={<CircleAlert size={22} />}
            title="暂时无法连接 HiMind AI"
            description={connectionError}
            actions={<>
              <button type="button" className="btn btn-primary" onClick={() => void connect()}><RefreshCw size={15} />重新连接</button>
              {connectionError.includes('运行时') ? <button type="button" className="btn" onClick={onOpenSettings}><Settings size={15} />打开设置</button> : null}
            </>}
          />
        ) : (
          <WorkspaceStatus icon={<LoaderCircle className="spin" size={21} />} title="正在准备 HiMind AI" description="马上就好" />
        )}
      </div>
      <BuiltinAiExtensionsDialog
        open={extensionsOpen}
        dashboardEnabled={!independentMode}
        toolSummary={toolSummary}
        onClose={() => setExtensionsOpen(false)}
        onRuntimeChanged={() => {
          setSessionUrl('');
          setFrameLoaded(false);
          setConnectionError('');
        }}
        onToolContextChanged={onToolContextChanged}
        onOpenPlugins={() => { setExtensionsOpen(false); onOpenPlugins(); }}
        onOpenSkills={() => { setExtensionsOpen(false); onOpenSkills(); }}
      />
    </section>
  );
}

function RuntimeActivityPanel({ sessions, error, onRefresh }: { sessions: BuiltinAIRuntimeActivity[]; error: string; onRefresh: () => void }) {
  return <aside className="builtin-ai-activity-panel" aria-label="协同活动">
    <header><div><strong>协同活动</strong><span>Dashboard、钉钉与本机运行状态</span></div><button type="button" className="btn btn-icon" title="刷新活动" aria-label="刷新活动" onClick={onRefresh}><RefreshCw size={14} /></button></header>
    {error ? <div className="builtin-ai-activity-message error" role="alert">{error}</div> : null}
    {!error && !sessions.length ? <div className="builtin-ai-activity-message">尚未发现可追踪的 DSH 会话</div> : null}
    <div className="builtin-ai-activity-list">{sessions.map(activity => <div className="builtin-ai-activity-item" key={activity.session.id}>
      <div className="builtin-ai-activity-item-head"><span className={`builtin-ai-activity-dot ${activity.session.status}`} /><strong>{activity.conversation?.title || activity.session.provider}</strong><span>{runtimeSessionStatusLabel(activity.session.status)}</span></div>
      <code>{activity.session.provider_session_id}</code>
      <small>{activity.conversation ? `入口：${activity.endpoints?.map(endpoint => endpoint.channel).join('、') || '本机'}` : '等待首条活动关联会话'} · {formatActivityTime(activity.session.last_heartbeat_at)}</small>
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
    <span className="builtin-ai-state-icon"><LoaderCircle className="spin" size={21} /></span>
    <h3>{installation.message || `正在${action} HiMind AI 运行时`}</h3>
    <p>{installation.operation === 'uninstall' ? '卸载期间 HiMind AI 暂不可用。' : '完成后会自动进入 HiMind AI。'}</p>
    <div className="builtin-ai-runtime-progress" aria-label={`${action}进度 ${installation.progress_percent}%`}>
      <div className="builtin-ai-runtime-progress-head"><span>{runtimeStageLabel(installation.stage)}</span><strong>{installation.progress_percent}%</strong></div>
      <div className="builtin-ai-runtime-progress-track"><span style={{ width: `${installation.progress_percent}%` }} /></div>
    </div>
  </div>;
}

function runtimeStageLabel(stage: string) {
  if (stage === 'resolving') return '检查安装包';
  if (stage === 'downloading') return '下载运行时';
  if (stage === 'verifying') return '校验运行时';
  if (stage === 'installing') return '安装运行时';
  if (stage === 'uninstalling') return '卸载运行时';
  return '正在准备';
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
  if (normalized.includes('independent mode')
    || normalized.includes('dsh 原生')
    || normalized.includes('settings.yaml')
    || (normalized.includes('provider') && normalized.includes('原生服务配置'))) {
    return '请先完成 DSH 原生 Provider 配置（settings.yaml），再开始对话。';
  }
  if (normalized.includes('登录 himind')) return '当前登录状态已失效，请重新登录。';
  if (normalized.includes('ai 服务')) return '当前账号暂未分配可用的 AI 服务，请联系管理员。';
  if (normalized.includes('运行时') && normalized.includes('修复')) return 'HiMind AI 运行时需要修复，请在设置中处理。';
  if ((normalized.includes('运行时') || normalized.includes('组件')) && normalized.includes('安装')) return '请先安装 HiMind AI 运行时，再开始对话。';
  if (normalized.includes('正在启动')) return '会话仍在准备中，请稍后重新连接。';
  return '服务暂时不可用，请稍后重试。';
}
