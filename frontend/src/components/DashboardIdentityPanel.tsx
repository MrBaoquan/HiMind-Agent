import { ArrowUpRight, CheckCircle2, CircleAlert, LogIn, LogOut, RefreshCw, X } from 'lucide-react';
import type { DashboardAuthorizationProgress, DashboardIdentityStatus } from '../services/agentApi';

type DashboardIdentityPanelProps = {
  identity: DashboardIdentityStatus | null;
  authorization: DashboardAuthorizationProgress | null;
  workerOnline: boolean;
  dashboardEnabled: boolean;
  workerStatusTitle: string;
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
  workerStatusTitle,
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
  const name = identity?.user_name || identity?.user_id || '尚未登录工作台';
  const ready = dashboardEnabled && workerOnline && identity?.authorized;
  const statusLabel = !dashboardEnabled ? '未启用' : ready ? '已就绪' : identity?.authorized ? workerStatusTitle : '需要登录';
  const statusDescription = ready
    ? '账号已登录 · 本机 Agent 已就绪'
    : !dashboardEnabled
      ? '独立模式下 Dashboard 服务未启用。'
      : identity?.authorized
      ? workerHealthDescription
      : identityDescription(identity);
  return (
    <section className={`workspace-status-panel ${ready ? 'ready' : 'attention'}`} id="account-authorization">
      <div className="workspace-status-body">
        <div className={`workspace-status-icon ${ready ? 'ready' : 'attention'}`} aria-hidden="true">
          {ready ? <CheckCircle2 size={25} /> : <CircleAlert size={25} />}
        </div>
        <div className="workspace-status-copy">
          <div className="workspace-status-kicker"><span>HiMind 工作台</span><span className={`workspace-status-pill ${ready ? 'ready' : 'attention'}`}><i />{statusLabel}</span></div>
          <strong>{name}</strong>
          <span>{statusDescription}</span>
        </div>
        <div className="identity-actions">
          <button className="btn btn-icon" title="刷新账号状态" aria-label="刷新账号状态" disabled={busy} onClick={onRefresh}><RefreshCw size={15} /></button>
          {dashboardEnabled ? (identity?.authorized ? <button className="btn btn-danger-quiet" disabled={busy || flowActive} onClick={onRevoke}><LogOut size={15} />退出登录</button> : <button className="btn btn-primary" title={authorizationDisabledReason} disabled={busy || flowActive || identity?.state === 'not_enrolled' || Boolean(authorizationDisabledReason)} onClick={onStartAuthorization}><LogIn size={15} />登录 HiMind</button>) : null}
        </div>
      </div>
      <div className="workspace-status-metrics" aria-label="Agent 运行状态">
        <div><span>待审批</span><strong className={pendingApprovals ? 'warning-text' : ''}>{pendingApprovals}</strong></div>
        <div><span>远程任务</span><strong>{remoteExecutionEnabled ? '已开启' : '已关闭'}</strong></div>
        <div><span>AI 工具</span><strong>{aiToolSummary}</strong></div>
      </div>
      {flowActive ? (
        <div className="authorization-flow">
          <div className="authorization-status">
            <span className="spinner" />
            <div><strong>{authorization?.state === 'starting' ? '正在打开登录页面' : '请在浏览器中确认'}</strong><span>{authorization?.user_code ? `确认码 ${authorization.user_code}` : '正在连接 HiMind 工作台'}</span></div>
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
  if (identity.state === 'not_enrolled') return '请先从工作台安装或重新连接 HiMind Agent';
  if (identity.state === 'expired') return '登录已过期，需要重新登录';
  if (identity.state === 'requires_login') return '登录已失效，需要重新登录';
  return '当前无法使用工作台功能';
}
