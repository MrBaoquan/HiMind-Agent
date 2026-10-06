import { ArrowUpRight, CheckCircle2, CircleAlert, Download, RefreshCw, Sparkles } from 'lucide-react';
import { LocalUsagePanel } from '../components/LocalUsagePanel';
import { BusyIndicator } from '../components/BusyIndicator';
import { PageHeader, Pill } from '../components/Common';
import { DashboardIdentityPanel } from '../components/DashboardIdentityPanel';
import type { AgentStatus, AgentUpdateStatus, AiUsageRange, ApprovalItem, DashboardAuthorizationProgress, DashboardIdentityStatus, InferenceGatewayStatus, LocalUsageOverview, McpTargetDescriptor, ProjectionSyncStatus, RemoteExecutionSettings } from '../services/agentApi';

type DashboardPageProps = {
  status: AgentStatus | null;
  projectionSyncStatus: ProjectionSyncStatus | null;
  approvals: ApprovalItem[];
  remoteExecutionSettings: RemoteExecutionSettings | null;
  mcpTargets: McpTargetDescriptor[];
  localUsage: LocalUsageOverview | null;
  localUsageRange: AiUsageRange;
  localUsageBusy: boolean;
  inferenceGateway: InferenceGatewayStatus | null;
  identity: DashboardIdentityStatus | null;
  authorization: DashboardAuthorizationProgress | null;
  identityBusy: boolean;
  updateStatus: AgentUpdateStatus | null;
  updateBusy: boolean;
  projectionRequeueBusy: boolean;
  onOpenDashboard: () => void;
  onRequeueProjectionDeadLetters: () => void;
  onStartAuthorization: () => void;
  onCancelAuthorization: () => void;
  onOpenAuthorization: () => void;
  onRefreshIdentity: () => void;
  onRevokeAuthorization: () => void;
  onLocalUsageRangeChange: (range: AiUsageRange) => void;
  onRefreshLocalUsage: () => void;
  onBindCodexToGateway: () => void;
  bindingModeBusy: boolean;
  onCheckUpdate: () => void;
  onDownloadUpdate: () => void;
  onInstallUpdate: () => void;
};

export function DashboardPage({
  status,
  projectionSyncStatus,
  approvals,
  remoteExecutionSettings,
  mcpTargets,
  localUsage,
  localUsageRange,
  localUsageBusy,
  inferenceGateway,
  identity,
  authorization,
  identityBusy,
  updateStatus,
  updateBusy,
  projectionRequeueBusy,
  onOpenDashboard,
  onRequeueProjectionDeadLetters,
  onStartAuthorization,
  onCancelAuthorization,
  onOpenAuthorization,
  onRefreshIdentity,
  onRevokeAuthorization,
  onLocalUsageRangeChange,
  onRefreshLocalUsage,
  onBindCodexToGateway,
  bindingModeBusy,
  onCheckUpdate,
  onDownloadUpdate,
  onInstallUpdate,
}: DashboardPageProps) {
  if (!status) {
    return <div className="page-loading"><BusyIndicator size={15} />正在读取应用状态</div>;
  }

  const independentMode = status.mode === 'independent' || status.dashboard_enabled === false;
  const workerExpected = status.dashboard_worker_expected ?? status.dashboard_enabled !== false;
  const workerOnline = !workerExpected || status.dashboard_worker_online;
  if (independentMode) {
    return (
      <div className="dashboard-page">
        <PageHeader title="首页" />
        {updateStatus && updateStatus.status !== 'idle' ? <AgentUpdateBanner status={updateStatus} busy={updateBusy} onCheck={onCheckUpdate} onDownload={onDownloadUpdate} onInstall={onInstallUpdate} /> : null}
        <ProjectionStatusPanel status={projectionSyncStatus} requeueBusy={projectionRequeueBusy} onRequeue={onRequeueProjectionDeadLetters} />
        <section className="workspace-status-panel ready independent-status-panel">
          <div className="workspace-status-body">
            <div className="workspace-status-icon ready" aria-hidden="true"><Sparkles size={25} /></div>
            <div className="workspace-status-copy">
              <div className="workspace-status-kicker"><span>HiMind Agent</span><span className="workspace-status-pill ready"><i />独立运行</span></div>
              <strong>HiMind Agent 已就绪</strong>
              <span>HiMind AI、技能、插件和已连接工具均可使用。</span>
            </div>
          </div>
          <div className="workspace-status-metrics" aria-label="本机运行状态">
            <div><span>待审批</span><strong>{approvals.length}</strong></div>
            <div><span>本机服务</span><strong>已就绪</strong></div>
            <div><span>AI 工具</span><strong>{mcpTargets.filter(target => target.id !== 'himind-ai' && target.detected && target.state === 'configured').length} 已连接</strong></div>
          </div>
        </section>
        <LocalUsagePanel
          overview={localUsage}
          gateway={inferenceGateway}
          range={localUsageRange}
          busy={localUsageBusy}
          onRangeChange={onLocalUsageRangeChange}
          onRefresh={onRefreshLocalUsage}
          onBindGateway={onBindCodexToGateway}
          bindBusy={bindingModeBusy}
        />
        <section className="overview-facts" aria-label="运行信息">
          <div><span>版本</span><strong>v{status.version}</strong></div>
          <div><span>AI 工作台</span><strong>未连接</strong></div>
          <div><span>本机服务</span><strong>运行中</strong></div>
          <div><span>当前任务</span><strong>{status.current_task ? '执行中' : '无任务'}</strong></div>
        </section>
      </div>
    );
  }
  const workerIssue = describeWorkerIssue(status.dashboard_worker_error, status.dashboard_worker_reason_code);
  const aiReadyCount = mcpTargets.filter(target => target.id !== 'himind-ai' && target.detected && target.state === 'configured').length;
  const aiInstalledCount = mcpTargets.filter(target => target.id !== 'himind-ai' && target.detected).length;
  return (
    <div className="dashboard-page">
      <PageHeader
        title="首页"
        description="从这里开始使用 AI 对话、自动化工作流和能力拓展。"
        actions={<button className="btn btn-primary" onClick={onOpenDashboard}><ArrowUpRight size={16} />打开工作台</button>}
      />
      {updateStatus && updateStatus.status !== 'idle' ? <AgentUpdateBanner status={updateStatus} busy={updateBusy} onCheck={onCheckUpdate} onDownload={onDownloadUpdate} onInstall={onInstallUpdate} /> : null}
      <ProjectionStatusPanel status={projectionSyncStatus} requeueBusy={projectionRequeueBusy} onRequeue={onRequeueProjectionDeadLetters} />
      <DashboardIdentityPanel
        identity={identity}
        authorization={authorization}
        workerOnline={workerOnline}
        dashboardEnabled={status.dashboard_enabled !== false}
        workerAttention={workerIssue.attention}
        workerIssueTitle={workerIssue.title}
        workerHealthDescription={workerIssue.healthDescription}
        pendingApprovals={approvals.length}
        remoteExecutionEnabled={Boolean(remoteExecutionSettings?.enabled)}
        aiToolSummary={aiInstalledCount ? `${aiReadyCount}/${aiInstalledCount} 已连接` : '未安装'}
        busy={identityBusy}
        onStartAuthorization={onStartAuthorization}
        onCancelAuthorization={onCancelAuthorization}
        onOpenAuthorization={onOpenAuthorization}
        onRefresh={onRefreshIdentity}
        onRevoke={onRevokeAuthorization}
        authorizationDisabledReason={workerIssue.requiresEnrollment ? workerIssue.description : undefined}
      />
      <LocalUsagePanel
        overview={localUsage}
        gateway={inferenceGateway}
        range={localUsageRange}
        busy={localUsageBusy}
        onRangeChange={onLocalUsageRangeChange}
        onRefresh={onRefreshLocalUsage}
        onBindGateway={onBindCodexToGateway}
        bindBusy={bindingModeBusy}
      />
      <section className="overview-facts" aria-label="运行信息">
        <div><span>版本</span><strong>v{status.version}</strong></div>
        {/* 这一格只回答「账号现在能不能用工作台」：写死「已对接」会和上面
            同时出现的「需要登录」自相矛盾。 */}
        <div><span>AI 工作台</span><strong>{identity?.authorized ? '已授权' : '未连接'}</strong></div>
        <div><span>本机服务</span><strong>运行中</strong></div>
        <div><span>当前任务</span><strong>{status.current_task ? '执行中' : '无任务'}</strong></div>
      </section>
    </div>
  );
}

// 原因文本来自后端错误日志：换行会把面板撑成多行，超长的 JSON 片段会挤掉右侧指标与操作。
function summarizeProjectionReason(raw: string): string {
  const compact = raw.replace(/\s+/g, ' ').trim();
  const text = projectionReasonText(compact) ?? compact;
  return text.length > 64 ? `${text.slice(0, 64)}…` : text;
}

/**
 * 后端把工作台的原始返回整段带回来了（英文 + JSON 片段），直接摆在面板上读不出结论。
 * 能归类的给一句中文；认不出来的一律返回 null，原文照旧展示，不猜也不藏。
 */
function projectionReasonText(raw: string): string | null {
  const text = raw.toLowerCase();
  if (/http 401|unauthorized/.test(text)) return '账号授权已失效，重新授权后会自动重投';
  if (/http 403|forbidden/.test(text)) return '工作台没有放行这次同步，先确认账号权限';
  if (/http 404/.test(text)) return '工作台没有这个同步接口（HTTP 404）';
  if (/http 409|conflict/.test(text)) return '工作台已有同号记录，两边数据冲突';
  if (/http 400|bad request/.test(text)) return '工作台拒绝接收这条记录（HTTP 400）';
  if (/timed? ?out|timeout/.test(text)) return '连接工作台超时';
  if (/dns|connect|network/.test(text)) return '连不上工作台，检查网络或工作台地址';
  return null;
}

function ProjectionStatusPanel({ status, requeueBusy, onRequeue }: { status: ProjectionSyncStatus | null; requeueBusy: boolean; onRequeue: () => void }) {
  if (!status) return null;
  const pending = status.pending ?? 0;
  const retrying = status.retrying ?? 0;
  const deadLetter = status.dead_letter ?? 0;
  const projected = status.projected ?? 0;
  const tone = status.state === 'attention' ? 'error' : status.state === 'pending' ? 'warning' : status.state === 'synced' ? 'success' : 'neutral';
  const title = deadLetter
    ? '同步需要处理'
    : status.state === 'local_only'
      ? '仅保存在本机'
      : status.state === 'pending'
        ? '等待同步'
        : '已同步';
  // 只报最主要的一类原因：多数死信来自同一个错误，逐条罗列反而看不清主要矛盾。
  const primaryReason = status.dead_letter_reasons?.[0];
  const reasonText = primaryReason ? summarizeProjectionReason(primaryReason.last_error) : '';
  // 归类后的中文只用来读，原文留在 title 里，排查时一个字段都不少。
  const reasonDetail = primaryReason ? primaryReason.last_error.replace(/\s+/g, ' ').trim() : '';
  const description = deadLetter
    ? `${deadLetter} 条任务同步失败，本地运行不受影响${reasonText ? `；主要原因是「${reasonText}」` : ''}`
    : status.state === 'local_only'
      ? `本地已记录 ${status.total} 条运行记录，对接 AI 工作台后会自动同步`
      : pending
        ? `${pending} 条等待同步${retrying ? `，其中 ${retrying} 条正在重试` : ''}`
        : `本地任务已同步 ${projected} 条`;
  return (
    <section className={`projection-sync-panel ${deadLetter && tone !== 'error' ? `${tone} error` : tone}`}>
      <div className="projection-sync-main">
        {tone === 'success' ? <CheckCircle2 size={18} /> : tone === 'error' ? <CircleAlert size={18} /> : <RefreshCw size={18} />}
        <div><strong>{title}</strong><span title={reasonDetail || undefined}>{description}</span></div>
      </div>
      <div className="projection-sync-metrics">
        <div><span>待同步</span><strong>{pending}</strong></div>
        <div><span>已同步</span><strong>{projected}</strong></div>
        <div><span>同步失败</span><strong>{deadLetter}</strong></div>
      </div>
      {deadLetter ? (
        <button className="btn" onClick={onRequeue} disabled={requeueBusy}>
          {requeueBusy ? <BusyIndicator size={14} /> : <RefreshCw size={14} />}
          重新同步
        </button>
      ) : null}
    </section>
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
      <div className="agent-update-icon">{downloading || checking ? <BusyIndicator size={19} /> : ready || status.status === 'rolled_back' ? <CheckCircle2 size={19} /> : <Download size={19} />}</div>
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

function describeWorkerIssue(error?: string, reasonCode?: string) {
  const value = error?.trim() || '';
  const normalized = value.toLowerCase();
  if (reasonCode === 'connected_agent_app_starting') {
    return {
      title: '正在连接工作台',
      description: '桌面端正在建立任务连接，请稍候刷新状态。',
      healthDescription: '本机服务正在建立工作台任务连接。',
      requiresEnrollment: false,
      attention: false,
    };
  }
  if (reasonCode === 'connected_agent_app_worker_error' && !value) {
    return {
      title: '工作台连接需要处理',
      description: '请刷新状态；若问题持续，请从工作台重新连接桌面端。',
      healthDescription: '本机服务仍在运行，但工作台任务连接尚未就绪。',
      requiresEnrollment: false,
      attention: true,
    };
  }
  if (
    normalized.includes('credential is no longer valid')
    || normalized.includes('authorize a new enrollment')
    || normalized.includes('invalid agent credentials')
  ) {
    return {
      title: '桌面端需要重新连接工作台',
      description: '这台电脑的设备凭证已失效。请回到 AI 工作台重新绑定设备。',
      healthDescription: '本机服务运行正常，但设备身份已失效，需要从工作台重新绑定。',
      requiresEnrollment: true,
      attention: true,
    };
  }
  if (normalized.includes('missing scope') || normalized.includes('required scope')) {
    return {
      title: '桌面端授权需要更新',
      description: '请在下方重新登录并授权工作台账号。',
      healthDescription: '本机服务运行正常，但当前账号授权范围不足。',
      requiresEnrollment: false,
      attention: true,
    };
  }
  if (
    normalized.includes('connection refused')
    || normalized.includes('timed out')
    || normalized.includes('dns')
    || normalized.includes('connect error')
  ) {
    return {
      title: '无法连接 AI 工作台',
      description: '请检查网络连接或高级信息中的工作台地址，稍后再试。',
      healthDescription: '本机服务仍在运行，但暂时无法访问工作台。',
      requiresEnrollment: false,
      attention: true,
    };
  }
  return {
    title: '工作台连接需要处理',
    description: '请刷新状态；若问题持续，请从工作台重新连接桌面端。',
    healthDescription: '本机服务仍在运行，但工作台任务连接尚未就绪。',
    requiresEnrollment: false,
    attention: true,
  };
}
