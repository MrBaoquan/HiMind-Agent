import { ArrowUpRight, CheckCircle2, CircleAlert, FolderOpen, Settings2, ShieldCheck } from 'lucide-react';
import { PageHeader, Pill } from '../components/Common';
import { DashboardIdentityPanel } from '../components/DashboardIdentityPanel';
import type { AgentStatus, ApprovalItem, ApprovalSettings, DashboardAuthorizationProgress, DashboardIdentityStatus, LoginState } from '../services/agentApi';

type DashboardPageProps = {
  status: AgentStatus | null;
  approvals: ApprovalItem[];
  settings: ApprovalSettings | null;
  loginState: LoginState | null;
  identity: DashboardIdentityStatus | null;
  authorization: DashboardAuthorizationProgress | null;
  identityBusy: boolean;
  onOpenDashboard: () => void;
  onOpenAgentDirectory: () => void;
  onOpenSettings: () => void;
  onStartAuthorization: () => void;
  onCancelAuthorization: () => void;
  onOpenAuthorization: () => void;
  onRefreshIdentity: () => void;
  onRevokeAuthorization: () => void;
};

export function DashboardPage({
  status,
  approvals,
  settings,
  loginState,
  identity,
  authorization,
  identityBusy,
  onOpenDashboard,
  onOpenAgentDirectory,
  onOpenSettings,
  onStartAuthorization,
  onCancelAuthorization,
  onOpenAuthorization,
  onRefreshIdentity,
  onRevokeAuthorization,
}: DashboardPageProps) {
  if (!status) {
    return <div className="page-loading"><span className="spinner" />正在读取 Agent 状态</div>;
  }

  const loginConfigured = loginState?.status === 'credentials_configured';
  const workerOnline = status.dashboard_worker_online;
  return (
    <>
      <PageHeader
        title="HiMind Agent"
        description="查看这台电脑与 HiMind 工作台的连接状态。"
        actions={<button className="btn btn-primary" onClick={onOpenDashboard}><ArrowUpRight size={16} />打开工作台</button>}
      />
      <DashboardIdentityPanel
        identity={identity}
        authorization={authorization}
        busy={identityBusy}
        onStartAuthorization={onStartAuthorization}
        onCancelAuthorization={onCancelAuthorization}
        onOpenAuthorization={onOpenAuthorization}
        onRefresh={onRefreshIdentity}
        onRevoke={onRevokeAuthorization}
      />
      {!workerOnline ? <div className="blocker"><CircleAlert size={18} /><div><strong>无法连接 HiMind 工作台</strong><span>请检查网络连接或工作台地址，稍后再试。</span></div></div> : null}
      <section className="health-panel">
        <div className={`health-icon ${workerOnline ? 'success' : 'danger'}`}>{workerOnline ? <CheckCircle2 size={25} /> : <CircleAlert size={25} />}</div>
        <div className="health-copy">
          <span className="eyebrow">运行状态</span>
          <h3>{workerOnline ? 'HiMind Agent 运行正常' : '连接需要处理'}</h3>
          <p>{workerOnline ? '这台电脑已连接 HiMind 工作台。' : '本机服务仍在运行，但暂时无法连接工作台。'}</p>
        </div>
        <div className="health-metrics">
          <div><span>待审批</span><strong className={approvals.length ? 'warning-text' : ''}>{approvals.length}</strong></div>
          <div><span>本地端口</span><strong>{status.local_port || 18181}</strong></div>
          <div><span>版本</span><strong>v{status.version}</strong></div>
        </div>
      </section>
      <div className="overview-grid overview-grid-single">
        <section className="card">
          <div className="card-header"><span>本机配置</span><Pill kind={loginConfigured ? 'success' : 'warn'}>{loginConfigured ? '账号已配置' : '待配置'}</Pill></div>
          <div className="card-body detail-list">
            <div className="detail-row"><span>内网账号</span><strong>{status.login_account || status.login_label || '未配置'}</strong></div>
            <div className="detail-row"><span>审批超时</span><strong>{settings?.timeout_seconds || 30} 秒</strong></div>
            <div className="detail-actions">
              <button className="btn" onClick={onOpenAgentDirectory}><FolderOpen size={16} />程序目录</button>
              <button className="btn" onClick={onOpenSettings}><Settings2 size={16} />配置 Agent</button>
            </div>
          </div>
        </section>
      </div>
      <div className="security-note"><ShieldCheck size={17} /><span>内网凭据仅保存在当前 Agent，本页面不会显示或传输密码。</span></div>
    </>
  );
}
