import { ArrowUpRight, CheckCircle2, CircleAlert, LogIn, LogOut, RefreshCw, Unplug, X } from 'lucide-react';
import type { DashboardAuthorizationProgress, DashboardIdentityStatus } from '../services/agentApi';
import { BusyIndicator } from './BusyIndicator';

/**
 * 连接已经坏掉的状态：这些才是「需要用户处理」的告警。
 * 从没连过工作台不算问题——本机功能不依赖工作台，按中性提示处理。
 */
const BROKEN_STATES = new Set(['requires_login', 'expired', 'dashboard_unavailable', 'insufficient_scope', 'disabled', 'invalid_local_authorization']);

type DashboardIdentityPanelProps = {
  identity: DashboardIdentityStatus | null;
  authorization: DashboardAuthorizationProgress | null;
  workerOnline: boolean;
  dashboardEnabled: boolean;
  /** 工作台任务连接是否到了需要用户处理的程度（正在建立连接不算）。 */
  workerAttention: boolean;
  workerIssueTitle: string;
  workerHealthDescription: string;
  pendingApprovals: number;
  remoteExecutionEnabled: boolean;
  aiToolSummary: string;
  busy?: boolean;
  onStartAuthorization: () => void;
  onCancelAuthorization: () => void;
  onOpenAuthorization: () => void;
  onRefresh: () => void;
  onRevoke: () => void;
  authorizationDisabledReason?: string;
};

export function DashboardIdentityPanel({
  identity,
  authorization,
  workerOnline,
  dashboardEnabled,
  workerAttention,
  workerIssueTitle,
  workerHealthDescription,
  pendingApprovals,
  remoteExecutionEnabled,
  aiToolSummary,
  busy,
  onStartAuthorization,
  onCancelAuthorization,
  onOpenAuthorization,
  onRefresh,
  onRevoke,
  authorizationDisabledReason,
}: DashboardIdentityPanelProps) {
  const flowActive = authorization?.state === 'starting' || authorization?.state === 'pending';
  const name = identity?.user_name || identity?.user_id || '未连接工作台';
  const authorized = Boolean(identity?.authorized);
  const ready = dashboardEnabled && workerOnline && authorized;
  // 三档语气：已就绪（绿）/ 连接真的坏了（橙，需处理）/ 未连接（中性灰）。
  const tone: 'ready' | 'attention' | 'neutral' = (() => {
    if (ready) return 'ready';
    if (!dashboardEnabled) return 'neutral';
    const broken = authorized ? workerAttention : BROKEN_STATES.has(identity?.state ?? '');
    return broken ? 'attention' : 'neutral';
  })();
  const statusLabel = !dashboardEnabled
    ? '未启用'
    : ready
      ? '已就绪'
      : authorized
        ? workerAttention ? '连接异常' : '连接中'
        : identityLabel(identity);
  const statusDescription = ready
    ? '账号已登录 · 桌面端已就绪'
    : !dashboardEnabled
      ? '未对接 AI 工作台。'
      : authorized
      ? workerHealthDescription
      : identityDescription(identity);
  return (
    <section className={`workspace-status-panel ${tone}`} id="account-authorization">
      <div className="workspace-status-body">
        <div className={`workspace-status-icon ${tone}`} aria-hidden="true">
          {ready ? <CheckCircle2 size={25} /> : tone === 'attention' ? <CircleAlert size={25} /> : <Unplug size={25} />}
        </div>
        <div className="workspace-status-copy">
          <div className="workspace-status-kicker"><span>AI 工作台</span><span className={`workspace-status-pill ${tone}`} title={!ready && authorized ? workerIssueTitle : undefined}><i />{statusLabel}</span></div>
          <strong>{name}</strong>
          <span>{statusDescription}</span>
        </div>
        <div className="identity-actions">
          <button className="btn btn-icon" title="刷新账号状态" aria-label="刷新账号状态" disabled={busy} onClick={onRefresh}><RefreshCw size={15} /></button>
          {dashboardEnabled ? (identity?.authorized ? <button className="btn btn-danger-quiet" disabled={busy || flowActive} onClick={onRevoke}><LogOut size={15} />取消授权</button> : <button className="btn btn-primary" title={authorizationDisabledReason} disabled={busy || flowActive || identity?.state === 'not_enrolled' || Boolean(authorizationDisabledReason)} onClick={onStartAuthorization}><LogIn size={15} />授权</button>) : null}
        </div>
      </div>
      <div className="workspace-status-metrics" aria-label="桌面端运行状态">
        <div><span>待审批</span><strong className={pendingApprovals ? 'warning-text' : ''}>{pendingApprovals}</strong></div>
        <div><span>远程任务</span><strong>{remoteExecutionEnabled ? '已开启' : '已关闭'}</strong></div>
        <div><span>AI 工具</span><strong>{aiToolSummary}</strong></div>
      </div>
      {flowActive ? (
        <div className="authorization-flow">
          <div className="authorization-status">
            <BusyIndicator size={15} />
            <div><strong>{authorization?.state === 'starting' ? '正在打开登录页面' : '请在浏览器中确认'}</strong><span>{authorization?.user_code ? `确认码 ${authorization.user_code}` : '正在连接 AI 工作台'}</span></div>
          </div>
          <div className="actions-row">
            {authorization?.verification_uri_complete ? <button className="btn" onClick={onOpenAuthorization}><ArrowUpRight size={15} />打开确认页面</button> : null}
            <button className="btn btn-icon" title="取消" aria-label="取消登录" onClick={onCancelAuthorization}><X size={15} /></button>
          </div>
        </div>
      ) : null}
    </section>
  );
}

function identityDescription(identity: DashboardIdentityStatus | null) {
  if (!identity) return '正在确认工作台账号';
  if (identity.state === 'authorized') {
    return identity.online_verified ? '账号已连接，工作台可用' : '账号已在这台电脑上登录';
  }
  if (identity.state === 'dashboard_unavailable') return '授权仍然有效，但暂时无法连接工作台';
  // 没连过工作台不是故障：本机 AI、能力与自动化都照常可用。
  if (identity.state === 'not_enrolled') return '这台电脑还没登记到工作台，需要时可以重新连接。';
  if (identity.state === 'expired') return '登录已过期，需要重新登录';
  if (identity.state === 'requires_login') return '登录已失效，需要重新登录';
  return '本机功能不受影响。';
}

/** 未连接时的短标签：只区分「从没连过」和「连过但坏了」，不写长句。 */
function identityLabel(identity: DashboardIdentityStatus | null) {
  if (!identity) return '读取中';
  if (identity.state === 'expired') return '授权已过期';
  if (identity.state === 'requires_login') return '需要重新登录';
  if (identity.state === 'insufficient_scope') return '权限不足';
  if (identity.state === 'disabled') return '账号已停用';
  if (identity.state === 'invalid_local_authorization') return '本地授权异常';
  if (identity.state === 'not_authorized') return '待授权';
  return '未连接';
}
