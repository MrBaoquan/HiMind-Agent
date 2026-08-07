import { ArrowUpRight, CheckCircle2, LogIn, LogOut, RefreshCw, ShieldCheck, X } from 'lucide-react';
import { Pill } from './Common';
import type { DashboardAuthorizationProgress, DashboardIdentityStatus } from '../services/agentApi';

type DashboardIdentityPanelProps = {
  identity: DashboardIdentityStatus | null;
  authorization: DashboardAuthorizationProgress | null;
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
  busy,
  onStartAuthorization,
  onCancelAuthorization,
  onOpenAuthorization,
  onRefresh,
  onRevoke,
  authorizationDisabledReason,
}: DashboardIdentityPanelProps) {
  const flowActive = authorization?.state === 'starting' || authorization?.state === 'pending';
  const [label, kind] = identityLabel(identity);
  const name = identity?.user_name || identity?.user_id || '尚未登录工作台';
  return (
    <section className="card identity-panel" id="account-authorization">
      <div className="card-header">
        <span>HiMind 工作台账号</span>
        <Pill kind={kind}>{label}</Pill>
      </div>
      <div className="identity-body">
        <div className={`identity-avatar ${identity?.authorized ? 'authorized' : ''}`}>
          {identity?.authorized ? <CheckCircle2 size={21} /> : <ShieldCheck size={21} />}
        </div>
        <div className="identity-copy">
          <strong>{name}</strong>
          <span>{identityDescription(identity)}</span>
        </div>
        <div className="identity-actions">
          <button className="btn btn-icon" title="刷新账号状态" aria-label="刷新账号状态" disabled={busy} onClick={onRefresh}><RefreshCw size={15} /></button>
          {identity?.authorized ? <button className="btn btn-danger-quiet" disabled={busy || flowActive} onClick={onRevoke}><LogOut size={15} />退出登录</button> : <button className="btn btn-primary" title={authorizationDisabledReason} disabled={busy || flowActive || identity?.state === 'not_enrolled' || Boolean(authorizationDisabledReason)} onClick={onStartAuthorization}><LogIn size={15} />登录 HiMind</button>}
        </div>
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
      {identity ? (
        <details className="identity-technical">
          <summary>账号详情</summary>
          <div className="identity-technical-grid">
            <div><span>工作台地址</span><code>{identity.dashboard_base || '--'}</code></div>
            <div><span>Agent ID</span><code>{identity.agent_id || '--'}</code></div>
            <div><span>用户 ID</span><code>{identity.user_id || '--'}</code></div>
            <div><span>授权范围</span><code>{identity.scopes.length ? identity.scopes.join(' · ') : '--'}</code></div>
            <div><span>SVN 账号</span><code>{identity.svn_username ? `${identity.svn_username} · ${svnProvisioningLabel(identity.svn_provisioning_status)}` : '--'}</code></div>
            {identity.svn_provisioning_error ? <div><span>SVN 开通状态</span><code>{identity.svn_provisioning_error}</code></div> : null}
            {identity.error ? <div><span>故障详情</span><code>{identity.error}</code></div> : null}
          </div>
        </details>
      ) : null}
    </section>
  );
}

function svnProvisioningLabel(status: string) {
  return ({ ready: '已就绪', provisioning: '正在开通', waiting_admin_agent: '等待控制 Agent', failed: '等待重试', unmanaged: '未托管' } as Record<string, string>)[status] || status || '等待同步';
}

function identityLabel(identity: DashboardIdentityStatus | null): [string, 'success' | 'warn' | 'danger' | 'neutral'] {
  if (!identity) return ['读取中', 'neutral'];
  if (identity.state === 'authorized') return ['已登录', 'success'];
  if (identity.state === 'dashboard_unavailable') return ['暂时离线', 'warn'];
  if (identity.state === 'not_authorized') return ['未登录', 'warn'];
  if (identity.state === 'not_enrolled') return ['设备未绑定', 'danger'];
  if (identity.state === 'insufficient_scope') return ['权限不足', 'danger'];
  return ['需要处理', 'danger'];
}

function identityDescription(identity: DashboardIdentityStatus | null) {
  if (!identity) return '正在确认工作台账号';
  if (identity.state === 'authorized') {
    if (identity.svn_provisioning_status && identity.svn_provisioning_status !== 'ready') return `账号已登录，SVN 账号${svnProvisioningLabel(identity.svn_provisioning_status)}`;
    return identity.online_verified ? '账号状态正常' : '账号已在这台电脑上登录';
  }
  if (identity.state === 'dashboard_unavailable') return '授权仍然有效，但暂时无法连接工作台';
  if (identity.state === 'not_enrolled') return '请先从工作台安装或重新连接 HiMind Agent';
  if (identity.state === 'expired') return '登录已过期，需要重新登录';
  if (identity.state === 'requires_login') return '登录已失效，需要重新登录';
  return '当前无法使用工作台功能';
}
