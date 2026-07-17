import { ArrowUpRight, CheckCircle2, CircleAlert, FolderOpen, Settings2, ShieldCheck } from 'lucide-react';
import { PageHeader, Pill } from '../components/Common';
import type { AgentStatus, ApprovalItem, ApprovalSettings, LoginState } from '../services/agentApi';

type DashboardPageProps = {
  status: AgentStatus | null;
  approvals: ApprovalItem[];
  settings: ApprovalSettings | null;
  loginState: LoginState | null;
  onOpenDashboard: () => void;
  onOpenAgentDirectory: () => void;
  onOpenSettings: () => void;
};

export function DashboardPage({
  status,
  approvals,
  settings,
  loginState,
  onOpenDashboard,
  onOpenAgentDirectory,
  onOpenSettings,
}: DashboardPageProps) {
  if (!status) {
    return <div className="page-loading"><span className="spinner" />正在读取 Agent 状态</div>;
  }

  const loginConfigured = loginState?.status === 'credentials_configured';
  const workerOnline = status.dashboard_worker_online;
  return (
    <>
      <PageHeader
        title="Agent 总览"
        description="查看本机服务、Dashboard 连接和关键配置状态。"
        actions={<button className="btn btn-primary" onClick={onOpenDashboard}><ArrowUpRight size={16} />打开 Dashboard</button>}
      />
      {!workerOnline ? <div className="blocker"><CircleAlert size={18} /><div><strong>Dashboard Worker 未连接</strong><span>{status.dashboard_worker_error || '依赖任务调度的操作暂不可用，请检查 Dashboard 地址或网络连接。'}</span></div></div> : null}
      <section className="health-panel">
        <div className={`health-icon ${workerOnline ? 'success' : 'danger'}`}>{workerOnline ? <CheckCircle2 size={25} /> : <CircleAlert size={25} />}</div>
        <div className="health-copy">
          <span className="eyebrow">运行状态</span>
          <h3>{workerOnline ? 'Agent 运行正常' : 'Agent 需要处理'}</h3>
          <p>{workerOnline ? '本地服务与 Dashboard Worker 均已就绪。' : '本地服务仍在运行，但 Dashboard Worker 当前离线。'}</p>
        </div>
        <div className="health-metrics">
          <div><span>待审批</span><strong className={approvals.length ? 'warning-text' : ''}>{approvals.length}</strong></div>
          <div><span>本地端口</span><strong>{status.local_port || 18181}</strong></div>
          <div><span>版本</span><strong>v{status.version}</strong></div>
        </div>
      </section>
      <div className="overview-grid">
        <section className="card">
          <div className="card-header"><span>连接信息</span><Pill kind={workerOnline ? 'success' : 'danger'}>{workerOnline ? '已连接' : '未连接'}</Pill></div>
          <div className="card-body detail-list">
            <div className="detail-row"><span>Dashboard 地址</span><code>{status.dashboard_base || '--'}</code></div>
            <div className="detail-row"><span>Agent ID</span><code>{status.dashboard_agent_id || '--'}</code></div>
            <div className="detail-row"><span>运行模式</span><strong>{status.mode || 'local-app'}</strong></div>
          </div>
        </section>
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
