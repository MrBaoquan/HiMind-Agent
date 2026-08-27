import { ArrowUpRight, CheckCircle2, CircleAlert, Download, LoaderCircle, RefreshCw, Sparkles } from 'lucide-react';
import { PageHeader, Pill } from '../components/Common';
import { DashboardIdentityPanel } from '../components/DashboardIdentityPanel';
import type { AgentStatus, AgentUpdateStatus, ApprovalItem, DashboardAuthorizationProgress, DashboardIdentityStatus, McpTargetDescriptor, RemoteExecutionSettings } from '../services/agentApi';

type DashboardPageProps = {
  status: AgentStatus | null;
  approvals: ApprovalItem[];
  remoteExecutionSettings: RemoteExecutionSettings | null;
  mcpTargets: McpTargetDescriptor[];
  identity: DashboardIdentityStatus | null;
  authorization: DashboardAuthorizationProgress | null;
  identityBusy: boolean;
  updateStatus: AgentUpdateStatus | null;
  updateBusy: boolean;
  onOpenDashboard: () => void;
  onStartAuthorization: () => void;
  onCancelAuthorization: () => void;
  onOpenAuthorization: () => void;
  onRefreshIdentity: () => void;
  onRevokeAuthorization: () => void;
  onCheckUpdate: () => void;
  onDownloadUpdate: () => void;
  onInstallUpdate: () => void;
};

export function DashboardPage({
  status,
  approvals,
  remoteExecutionSettings,
  mcpTargets,
  identity,
  authorization,
  identityBusy,
  updateStatus,
  updateBusy,
  onOpenDashboard,
  onStartAuthorization,
  onCancelAuthorization,
  onOpenAuthorization,
  onRefreshIdentity,
  onRevokeAuthorization,
  onCheckUpdate,
  onDownloadUpdate,
  onInstallUpdate,
}: DashboardPageProps) {
  if (!status) {
    return <div className="page-loading"><span className="spinner" />正在读取 Agent 状态</div>;
  }

  const independentMode = status.mode === 'independent' || status.dashboard_enabled === false;
  const workerOnline = status.dashboard_worker_online;
  if (independentMode) {
    return (
      <div className="dashboard-page">
        <PageHeader title="概览" description="HiMind Agent 当前状态" />
        {updateStatus && updateStatus.status !== 'idle' ? <AgentUpdateBanner status={updateStatus} busy={updateBusy} onCheck={onCheckUpdate} onDownload={onDownloadUpdate} onInstall={onInstallUpdate} /> : null}
        <section className="workspace-status-panel ready independent-status-panel">
          <div className="workspace-status-body">
            <div className="workspace-status-icon ready" aria-hidden="true"><Sparkles size={25} /></div>
            <div className="workspace-status-copy">
              <div className="workspace-status-kicker"><span>HiMind Agent</span><span className="workspace-status-pill ready"><i />独立运行</span></div>
              <strong>本机 AI 工作区已就绪</strong>
              <span>HiMind AI、技能、插件、MCP 和本机执行能力均可直接使用。</span>
            </div>
          </div>
          <div className="workspace-status-metrics" aria-label="本机运行状态">
            <div><span>待审批</span><strong>{approvals.length}</strong></div>
            <div><span>本机服务</span><strong>已就绪</strong></div>
            <div><span>AI 工具</span><strong>{mcpTargets.filter(target => target.id !== 'himind-ai' && target.detected && target.state === 'configured').length} 已连接</strong></div>
          </div>
        </section>
        <section className="overview-facts" aria-label="运行信息">
          <div><span>版本</span><strong>v{status.version}</strong></div>
          <div><span>运行档</span><strong>{status.profile || 'production'}</strong></div>
          <div><span>本地服务</span><strong>:{status.local_port || 18181}</strong></div>
          <div><span>当前任务</span><strong>{status.current_task ? '执行中' : '无任务'}</strong></div>
        </section>
      </div>
    );
  }
  const workerIssue = describeWorkerIssue(status.dashboard_worker_error);
  const aiReadyCount = mcpTargets.filter(target => target.id !== 'himind-ai' && target.detected && target.state === 'configured').length;
  const aiInstalledCount = mcpTargets.filter(target => target.id !== 'himind-ai' && target.detected).length;
  return (
    <div className="dashboard-page">
      <PageHeader
        title="概览"
        description="HiMind Agent 当前状态"
        actions={<button className="btn btn-primary" onClick={onOpenDashboard}><ArrowUpRight size={16} />打开工作台</button>}
      />
      {updateStatus && updateStatus.status !== 'idle' ? <AgentUpdateBanner status={updateStatus} busy={updateBusy} onCheck={onCheckUpdate} onDownload={onDownloadUpdate} onInstall={onInstallUpdate} /> : null}
      {!workerOnline ? <div className="blocker"><CircleAlert size={18} /><div><strong>{workerIssue.title}</strong><span>{workerIssue.description}</span></div></div> : null}
      <DashboardIdentityPanel
        identity={identity}
        authorization={authorization}
        workerOnline={workerOnline}
        dashboardEnabled={status.dashboard_enabled !== false}
        workerStatusTitle={workerIssue.title}
        workerHealthDescription={workerIssue.healthDescription}
        pendingApprovals={approvals.length}
        remoteExecutionEnabled={Boolean(remoteExecutionSettings?.enabled)}
        aiToolSummary={aiInstalledCount ? `${aiReadyCount}/${aiInstalledCount} 已注册` : '未安装'}
        busy={identityBusy}
        onStartAuthorization={onStartAuthorization}
        onCancelAuthorization={onCancelAuthorization}
        onOpenAuthorization={onOpenAuthorization}
        onRefresh={onRefreshIdentity}
        onRevoke={onRevokeAuthorization}
        authorizationDisabledReason={workerIssue.requiresEnrollment ? workerIssue.description : undefined}
      />
      <section className="overview-facts" aria-label="运行信息">
        <div><span>版本</span><strong>v{status.version}</strong></div>
        <div><span>运行档</span><strong>{status.profile || 'production'}</strong></div>
        <div><span>本地服务</span><strong>:{status.local_port || 18181}</strong></div>
        <div><span>当前任务</span><strong>{status.current_task ? '执行中' : '无任务'}</strong></div>
      </section>
    </div>
  );
}

function AgentUpdateBanner({ status, busy, onCheck, onDownload, onInstall }: { status: AgentUpdateStatus; busy: boolean; onCheck: () => void; onDownload: () => void; onInstall: () => void }) {
  const downloading = status.status === 'downloading';
  const ready = status.status === 'ready';
  const checking = status.status === 'checking';
  const failed = status.status === 'failed';
  const title = status.status === 'installing'
    ? '正在重启并更新'
    : ready
    ? `v${status.available_version} 已准备就绪`
    : downloading
      ? `正在下载 v${status.available_version}`
      : checking
        ? '正在检查软件更新'
      : failed
        ? '软件更新需要处理'
        : status.status === 'rolled_back'
          ? '已恢复上一版本'
        : `发现新版本 v${status.available_version}`;
  return (
    <section className={`agent-update-banner${failed ? ' error' : ''}`}>
      <div className="agent-update-icon">{downloading || checking ? <LoaderCircle size={19} className="spin" /> : ready || status.status === 'rolled_back' ? <CheckCircle2 size={19} /> : <Download size={19} />}</div>
      <div className="agent-update-copy">
        <div><strong>{title}</strong>{status.mandatory ? <Pill kind="warn">重要更新</Pill> : null}</div>
        <span>{updateBannerMessage(status)}</span>
        {downloading ? <div className="agent-update-progress" aria-label={`下载进度 ${status.progress_percent}%`}><span style={{ width: `${status.progress_percent}%` }} /></div> : null}
      </div>
      <div className="agent-update-actions">
        {ready ? <button className="btn btn-primary" disabled={busy} onClick={onInstall}><RefreshCw size={15} />重启并更新</button> : null}
        {!ready && !downloading && status.available_version ? <button className="btn btn-primary" disabled={busy} onClick={onDownload}><Download size={15} />下载更新</button> : null}
        {failed ? <button className="btn" disabled={busy} onClick={onCheck}><RefreshCw size={15} />重新检查</button> : null}
      </div>
    </section>
  );
}

function updateBannerMessage(status: AgentUpdateStatus) {
  if (status.status === 'checking') return '正在获取更新信息。';
  if (status.status === 'downloading') return '下载完成后会通知你。';
  if (status.status === 'ready') return '重启 HiMind Agent 后完成安装。';
  if (status.status === 'installing') return '更新完成后会自动重新启动。';
  if (status.status === 'failed') return '暂时无法完成更新，请稍后重试。';
  if (status.status === 'rolled_back') return '更新没有完成，仍在使用上一版本。';
  return status.release_notes || '可以先下载，准备完成后再选择何时重启。';
}

function describeWorkerIssue(error?: string) {
  const value = error?.trim() || '';
  const normalized = value.toLowerCase();
  if (
    normalized.includes('credential is no longer valid')
    || normalized.includes('authorize a new enrollment')
    || normalized.includes('invalid agent credentials')
  ) {
    return {
      title: 'Agent 需要重新连接工作台',
      description: '这台电脑的设备凭证已失效。请回到 HiMind 工作台，在 Agent 状态中点击“重新绑定”。',
      healthDescription: '本机服务运行正常，但设备身份已失效，需要从工作台重新绑定。',
      requiresEnrollment: true,
    };
  }
  if (normalized.includes('missing scope') || normalized.includes('required scope')) {
    return {
      title: 'Agent 授权范围需要更新',
      description: '请在下方重新登录并授权工作台账号。',
      healthDescription: '本机服务运行正常，但当前账号授权范围不足。',
      requiresEnrollment: false,
    };
  }
  if (
    normalized.includes('connection refused')
    || normalized.includes('timed out')
    || normalized.includes('dns')
    || normalized.includes('connect error')
  ) {
    return {
      title: '无法连接 HiMind 工作台',
      description: '请检查网络连接或高级信息中的工作台地址，稍后再试。',
      healthDescription: '本机服务仍在运行，但暂时无法访问工作台。',
      requiresEnrollment: false,
    };
  }
  return {
    title: '工作台连接需要处理',
    description: '请刷新状态；若问题持续，请从工作台重新连接 Agent。',
    healthDescription: '本机服务仍在运行，但工作台任务连接尚未就绪。',
    requiresEnrollment: false,
  };
}
