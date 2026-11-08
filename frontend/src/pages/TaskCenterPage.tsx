import { useCallback, useEffect, useMemo, useState } from 'react';
import { Activity, AlertCircle, CheckCircle2, Clock3, ExternalLink, Search, XCircle } from 'lucide-react';
// 状态图标链吃的是图标数据：同一个图标位在等待/运行/完成之间形变。
import { CircleAlert, CircleCheck, CircleDashed, Clock, LoaderCircle as loaderIconData } from 'lucide';
import { BusyIndicator } from '../components/BusyIndicator';
import { EmptyState, PageHeader } from '../components/Common';
import { MorphIcon } from '../components/MorphIcon';
import type { AgentActivityItem, AgentTaskHistoryItem, CurrentTaskStatus } from '../services/agentApi';
import {
  TASK_ACTIVE_STATUSES,
  TASK_ATTENTION_STATUSES,
  TASK_COMPLETED_STATUSES,
  formatAbsoluteTime,
  formatDuration,
  formatRelativeTime,
  shortRunId,
  taskElapsedSeconds,
  taskProgressLabel,
  taskProgressValue,
  taskStatusLabel,
  taskStatusTone,
  taskStepLabel,
  taskTypeLabel,
  parentKindLabel,
  roleLabel,
  roleChips,
  childRuns,
  chainState,
  shouldShowRoleBadge,
  shouldShowRoleChain,
  teamSummary,
  type ChildRun,
  type RoleChip,
} from './taskView';
import { formatElapsedCn } from './workflowRunView';
import { formatError } from '../types';

type TaskFilter = 'all' | 'running' | 'completed' | 'attention';
type ActivitySource = 'dashboard' | 'workflow' | 'skill' | 'schedule';
type SourceFilter = 'all' | ActivitySource;

type TaskCenterPageProps = {
  currentTask: CurrentTaskStatus | null;
  dashboardEnabled: boolean;
  onLoadTaskHistory: () => Promise<AgentTaskHistoryItem[]>;
  onLoadLocalActivity: () => Promise<AgentActivityItem[]>;
  onOpenDashboard: () => void;
  onOpenWorkflowRun?: (runId: string) => void;
  /** 取消本机工作流运行；缺省时该操作不渲染。 */
  onCancelRun?: (runId: string) => Promise<void>;
  /** 批准/拒绝当前等待审批的步骤；缺省时这两个操作不渲染。 */
  onApproveRun?: (runId: string, stepId: string) => Promise<void>;
  onRejectRun?: (runId: string, stepId: string) => Promise<void>;
  onPauseRun?: (runId: string) => Promise<void>;
  onResumePausedRun?: (runId: string) => Promise<void>;
  /** 重试失败的步骤；缺省时不渲染该操作。 */
  onRetryStep?: (runId: string, stepId: string) => Promise<void>;
};

const ACTIVE_STATUSES = TASK_ACTIVE_STATUSES;
const COMPLETED_STATUSES = TASK_COMPLETED_STATUSES;
const ATTENTION_STATUSES = TASK_ATTENTION_STATUSES;

const FILTERS: { id: TaskFilter; label: string }[] = [
  { id: 'all', label: '全部' },
  { id: 'running', label: '进行中' },
  { id: 'completed', label: '已完成' },
  { id: 'attention', label: '需处理' },
];

const SOURCE_LABELS: Record<ActivitySource, string> = {
  dashboard: '工作台下发',
  workflow: '本机工作流',
  skill: '技能',
  schedule: '定时计划',
};

/** 列表项：把两种来源压平成同一套字段，行渲染只认这一种结构。 */
type ActivityEntry = {
  id: string;
  source: ActivitySource;
  sourceLabel: string;
  title: string;
  subtitle: string;
  status: string;
  /** null 表示这一条拿不到进度（本机技能运行），界面按「进行中」显示。 */
  progress: number | null;
  stepDone: number | null;
  stepTotal: number | null;
  detail: string;
  error: string;
  createdAt: string;
  startedAt: string;
  finishedAt: string;
  updatedAt: string;
  workflowRunId: string;
  /** 当前步骤的执行角色；空串表示没有角色信息。 */
  roleId: string;
  /** 父子语义：delegate / retry / 空。 */
  parentKind: string;
  /** 顶层运行的协作计数；没有可显示的协作时不渲染。 */
  team: { delegated: number; retries: number; running: number } | null;
  /** 这条运行出现过的角色（按步骤出现顺序），用于展示协作链路。 */
  roles: RoleChip[];
  /** 当前停留的步骤；行内审批按它定位。空串表示没有可审批步骤。 */
  currentStepId: string;
  /** 停下来的原因；只有 waiting 状态有意义。 */
  pauseKind: string;
  /** 失败的那一步；行内重试按它定位。空串表示没有失败步骤。 */
  failedStepId: string;
  /** 子运行明细；父行展开时渲染，空数组表示不渲染展开入口。 */
  childrenRuns: ChildRun[];
};

function sourceOf(value: string): ActivitySource {
  return value === 'workflow' || value === 'skill' || value === 'schedule' ? value : 'skill';
}

/**
 * 状态图标链：等待 → 运行 → 完成/失败共用同一个图标位。
 * 整行换一个图标读者要重新找位置，形变则把「任务往前走了一步」直接画出来。
 * `canceling` 归到运行态：取消中仍然在跑，只是准备停。
 */
function stateIcon(status: string) {
  if (status === 'pending') return CircleDashed;
  if (ACTIVE_STATUSES.has(status)) return loaderIconData;
  if (COMPLETED_STATUSES.has(status)) return CircleCheck;
  if (status === 'failed') return CircleAlert;
  return Clock;
}

function fromTask(item: AgentTaskHistoryItem): ActivityEntry {
  return {
    id: item.id,
    source: 'dashboard',
    sourceLabel: SOURCE_LABELS.dashboard,
    title: taskTypeLabel(item.task_type),
    subtitle: '工作台任务',
    status: item.status,
    progress: typeof item.progress === 'number' ? item.progress : null,
    stepDone: null,
    stepTotal: null,
    detail: item.detail || '',
    error: item.error || '',
    createdAt: item.created_at,
    startedAt: item.started_at || '',
    finishedAt: item.finished_at || '',
    updatedAt: item.updated_at,
    workflowRunId: '',
    // 工作台任务没有角色与父子关系：协作区保持为空，界面按旧行为渲染。
    roleId: '',
    parentKind: '',
    team: null,
    roles: [],
    currentStepId: '',
    pauseKind: '',
    failedStepId: '',
    childrenRuns: [],
  };
}

function fromActivity(item: AgentActivityItem): ActivityEntry {
  const source = sourceOf(item.source);
  return {
    id: item.id,
    source,
    sourceLabel: SOURCE_LABELS[source],
    title: item.title || item.id,
    subtitle: item.subtitle || '',
    status: item.status,
    progress: typeof item.progress === 'number' ? item.progress : null,
    stepDone: typeof item.step_done === 'number' ? item.step_done : null,
    stepTotal: typeof item.step_total === 'number' ? item.step_total : null,
    detail: item.detail || '',
    error: item.error || '',
    createdAt: item.created_at,
    startedAt: item.started_at || '',
    finishedAt: item.finished_at || '',
    updatedAt: item.updated_at || item.created_at,
    workflowRunId: item.workflow_run_id || '',
    roleId: (item.role_id || '').trim(),
    parentKind: (item.parent_kind || '').trim(),
    team: teamSummary(item.team),
    roles: roleChips(item.roles),
    currentStepId: (item.current_step_id || '').trim(),
    pauseKind: (item.pause_kind || '').trim(),
    failedStepId: (item.failed_step_id || '').trim(),
    childrenRuns: childRuns(item.children),
  };
}

function activityTime(entry: ActivityEntry) {
  return entry.finishedAt || entry.updatedAt || entry.createdAt;
}

export function TaskCenterPage({ currentTask, dashboardEnabled, onLoadTaskHistory, onLoadLocalActivity, onOpenDashboard, onOpenWorkflowRun, onCancelRun, onApproveRun, onRejectRun, onPauseRun, onResumePausedRun, onRetryStep }: TaskCenterPageProps) {
  const [items, setItems] = useState<ActivityEntry[]>([]);
  const [filter, setFilter] = useState<TaskFilter>('all');
  const [sourceFilter, setSourceFilter] = useState<SourceFilter>('all');
  const [query, setQuery] = useState('');
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [updatedAt, setUpdatedAt] = useState<Date | null>(null);

  // 本机记录是本地读取（快），工作台历史要打控制面接口（慢）。轮询按两个节奏走：
  // 本机 5 秒，工作台历史 30 秒，避免每 5 秒发一次网络请求。
  const load = useCallback(async (silent = false, includeRemote = true) => {
    if (!silent) setLoading(true);
    const [local, remote] = await Promise.allSettled([
      onLoadLocalActivity(),
      dashboardEnabled && includeRemote ? onLoadTaskHistory() : Promise.resolve([] as AgentTaskHistoryItem[]),
    ]);
    const next: ActivityEntry[] = [];
    if (local.status === 'fulfilled') next.push(...local.value.map(fromActivity));
    if (remote.status === 'fulfilled' && includeRemote) next.push(...remote.value.map(fromTask));
    next.sort((left, right) => Date.parse(activityTime(right)) - Date.parse(activityTime(left)));
    // 未拉取工作台历史时，保留上一轮的记录，避免列表被清空后闪回。
    setItems(current => includeRemote ? next : [...next, ...current.filter(item => item.source === 'dashboard')]);
    const failed = [local, remote].filter(result => result.status === 'rejected').length;
    if (includeRemote) {
      setError(failed === 0 ? '' : failed === 2 ? '记录暂时读取失败，请稍后重试。' : '部分来源暂时读取失败，可稍后重试。');
    } else if (local.status === 'rejected') {
      setError('本机记录暂时读取失败，可稍后重试。');
    }
    setUpdatedAt(new Date());
    if (!silent) setLoading(false);
  }, [dashboardEnabled, onLoadLocalActivity, onLoadTaskHistory]);

  useEffect(() => {
    void load();
    let tick = 0;
    const timer = window.setInterval(() => {
      if (document.visibilityState === 'hidden') return;
      tick += 1;
      void load(true, tick % 6 === 0);
    }, 5000);
    return () => window.clearInterval(timer);
  }, [load]);

  const active = items.filter(item => ACTIVE_STATUSES.has(item.status));
  const completed = items.filter(item => COMPLETED_STATUSES.has(item.status));
  const attention = items.filter(item => ATTENTION_STATUSES.has(item.status));
  // 有任务在跑就按秒推进「现在」：进行中的行显示走动的「已运行」，相对时间也跟着走。
  // 没有活动运行时不挂定时器，空转会让整页无谓重渲染。
  const [nowMs, setNowMs] = useState(() => Date.now());
  useEffect(() => {
    if (active.length === 0) return;
    setNowMs(Date.now());
    const timer = window.setInterval(() => setNowMs(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [active.length]);
  const currentItem = currentTask ? items.find(item => item.id === currentTask.task_id) : active[0];
  const availableSources = useMemo(() => {
    const present = new Set(items.map(item => item.source));
    return (Object.keys(SOURCE_LABELS) as ActivitySource[]).filter(source => present.has(source));
  }, [items]);
  const filteredItems = useMemo(() => {
    const normalizedQuery = query.trim().toLowerCase();
    return items.filter(item => {
      const matchesFilter = filter === 'all'
        || (filter === 'running' && ACTIVE_STATUSES.has(item.status))
        || (filter === 'completed' && COMPLETED_STATUSES.has(item.status))
        || (filter === 'attention' && ATTENTION_STATUSES.has(item.status));
      if (!matchesFilter) return false;
      if (sourceFilter !== 'all' && item.source !== sourceFilter) return false;
      if (!normalizedQuery) return true;
      return [item.id, item.title, item.subtitle, item.detail, item.error]
        .filter(Boolean)
        .some(value => String(value).toLowerCase().includes(normalizedQuery));
    });
  }, [filter, items, query, sourceFilter]);

  return (
    <div className="task-center-page">
      <PageHeader
        title="我的任务"
        description={dashboardEnabled ? '查看 AI 对话、自动化工作流和工作台发起的任务。' : '查看 AI 对话和本机自动化工作流发起的任务。'}
        actions={dashboardEnabled ? <button type="button" className="btn btn-primary" onClick={onOpenDashboard}><ExternalLink size={15} />打开工作台</button> : undefined}
      />

      <section className="task-center-summary" aria-label="活动概览">
        <SummaryItem label="进行中" value={active.length} tone="running" />
        <SummaryItem label="需处理" value={attention.length} tone={attention.length ? 'attention' : 'neutral'} />
        <SummaryItem label="已完成" value={completed.length} tone="success" />
        <SummaryItem label="记录总数" value={items.length} tone="neutral" />
      </section>

      {active.length > 0 ? (
        <section className="task-center-current" aria-labelledby="task-center-current-title">
          <div className="task-center-section-heading"><div><span className="task-center-section-kicker"><Clock3 size={14} />实时状态</span><h3 id="task-center-current-title">正在进行</h3></div><span>{active.length} 项</span></div>
          <div className="task-center-current-list">{active.map(item => <ActivityRow key={item.id} entry={item} featured={item.id === currentItem?.id} now={nowMs} onOpenWorkflowRun={onOpenWorkflowRun} onCancelRun={onCancelRun} onApproveRun={onApproveRun} onRejectRun={onRejectRun} onPauseRun={onPauseRun} onResumePausedRun={onResumePausedRun} />)}</div>
        </section>
      ) : null}

      <section className="task-center-history" aria-labelledby="task-center-history-title">
        <div className="task-center-section-heading">
          <div><span className="task-center-section-kicker"><CheckCircle2 size={14} />可回看</span><h3 id="task-center-history-title">运行记录</h3></div>
          <span>{loading ? <><BusyIndicator size={11} /> 更新中</> : updatedAt ? `${filteredItems.length} / ${items.length} · 更新于 ${updatedAt.toLocaleTimeString('zh-CN', { hour12: false })}` : `${filteredItems.length} / ${items.length}`}</span>
        </div>
        <div className="task-center-toolbar">
          <div className="task-center-filter-stack">
            <div className="task-center-filters" role="tablist" aria-label="状态筛选">
              {FILTERS.map(option => {
                const count = option.id === 'all' ? items.length : option.id === 'running' ? active.length : option.id === 'completed' ? completed.length : attention.length;
                return <button type="button" role="tab" aria-selected={filter === option.id} className={filter === option.id ? 'active' : ''} key={option.id} onClick={() => setFilter(option.id)}><span>{option.label}</span><small>{count}</small></button>;
              })}
            </div>
            {availableSources.length > 1 ? (
              <div className="task-center-filters sources" role="tablist" aria-label="来源筛选">
                <button type="button" role="tab" aria-selected={sourceFilter === 'all'} className={sourceFilter === 'all' ? 'active' : ''} onClick={() => setSourceFilter('all')}><span>全部来源</span><small>{items.length}</small></button>
                {availableSources.map(source => {
                  const count = items.filter(item => item.source === source).length;
                  return <button type="button" role="tab" aria-selected={sourceFilter === source} className={sourceFilter === source ? 'active' : ''} key={source} onClick={() => setSourceFilter(source)}><span>{SOURCE_LABELS[source]}</span><small>{count}</small></button>;
                })}
              </div>
            ) : null}
          </div>
          <label className="task-center-search"><Search size={15} aria-hidden="true" /><span className="sr-only">搜索记录</span><input value={query} onChange={event => setQuery(event.target.value)} placeholder="搜索名称、编号或结果" /></label>
        </div>
        {error ? <div className="task-center-alert" role="alert"><AlertCircle size={16} /><span>{error}</span><button type="button" className="btn" onClick={() => void load()}>重新读取</button></div> : null}
        {loading && items.length === 0
          ? <div className="task-center-loading"><BusyIndicator />正在读取运行记录</div>
          : filteredItems.length > 0
            ? <div className="task-center-list">{filteredItems.map(item => <ActivityRow key={item.id} entry={item} now={nowMs} onOpenWorkflowRun={onOpenWorkflowRun} onCancelRun={onCancelRun} onApproveRun={onApproveRun} onRejectRun={onRejectRun} onPauseRun={onPauseRun} onResumePausedRun={onResumePausedRun} />)}</div>
            : <div className="task-center-empty">
                <EmptyState
                  icon={filter === 'attention' ? XCircle : Activity}
                  title={items.length === 0 ? '还没有运行记录' : '没有匹配的记录'}
                  text={items.length === 0
                    ? dashboardEnabled
                      ? '工作台下发任务或本机运行工作流/技能后，记录会显示在这里。'
                      : '本机运行工作流或技能后，记录会显示在这里。'
                    : '可以换一个状态、来源或搜索关键词。'}
                />
                {items.length === 0 && dashboardEnabled ? <button type="button" className="btn btn-primary" onClick={onOpenDashboard}><ExternalLink size={15} />打开工作台发起任务</button> : null}
              </div>}
      </section>
    </div>
  );
}

function SummaryItem({ label, value, tone }: { label: string; value: number; tone: 'running' | 'attention' | 'success' | 'neutral' }) {
  // 有任务在跑时让这个数字一起呼吸：概览区也要能看出「现在是活的」。
  return <div className={`task-center-summary-item ${tone}${tone === 'running' && value > 0 ? ' is-live' : ''}`}><span>{label}</span><strong>{value}</strong></div>;
}

type RowActions = {
  onOpenWorkflowRun?: (runId: string) => void;
  onCancelRun?: (runId: string) => Promise<void>;
  onApproveRun?: (runId: string, stepId: string) => Promise<void>;
  onRejectRun?: (runId: string, stepId: string) => Promise<void>;
  onPauseRun?: (runId: string) => Promise<void>;
  onResumePausedRun?: (runId: string) => Promise<void>;
  onRetryStep?: (runId: string, stepId: string) => Promise<void>;
};

function ActivityRow({ entry, featured = false, now, onOpenWorkflowRun, onCancelRun, onApproveRun, onRejectRun, onPauseRun, onResumePausedRun, onRetryStep }: { entry: ActivityEntry; featured?: boolean; now: number } & RowActions) {
  const active = ACTIVE_STATUSES.has(entry.status);
  const tone = taskStatusTone(entry.status);
  const progress = taskProgressValue(entry.progress);
  const StatusIcon = stateIcon(entry.status);
  // 运行态一律转圈：进度已知时进度条是停的（只有进度未知才走不确定态条），
  // 只留一个静止的圈会被读成「卡住了」。转圈只在提醒「还在跑」，进度交给百分比。
  const spinning = active;
  const stepLabel = taskStepLabel(entry.stepDone, entry.stepTotal);
  const message = entry.error || entry.detail;
  const duration = formatDuration(entry.startedAt, entry.finishedAt || entry.updatedAt);
  // 进行中的行显示走动的「已运行」：这是「还活着」最直接的证据。
  const elapsed = active ? taskElapsedSeconds(entry.startedAt || entry.createdAt, now) : null;
  const team = entry.team;
  const parentKind = parentKindLabel(entry.parentKind);
  // 行内操作：只有在调用方提供了回调、且这条运行确实支持该动作时才渲染。
  // 不做「灰按钮」——那会让用户以为功能坏了。
  const [busy, setBusy] = useState<'cancel' | 'approve' | 'reject' | 'pause' | 'resume' | 'retry' | ''>('');
  const [actionError, setActionError] = useState('');
  const canCancel = Boolean(onCancelRun) && active && Boolean(entry.workflowRunId);
  // 暂停只对「正在跑」有意义；等审批/等反馈的运行不是「在跑」，暂停它没有语义。
  const canPause = Boolean(onPauseRun) && entry.status === 'running' && Boolean(entry.workflowRunId);
  // 恢复只对「用户主动暂停」出现，不能用来绕过审批或反馈等待。
  const canResume = Boolean(onResumePausedRun) && entry.status === 'waiting'
    && entry.pauseKind === 'user' && Boolean(entry.workflowRunId);
  const awaitingApproval = entry.status === 'waiting' && entry.pauseKind !== 'user';
  const canDecide = Boolean(onApproveRun && onRejectRun) && awaitingApproval && Boolean(entry.workflowRunId);
  // 重试只对失败且已停下的运行出现：跑着的时候重跑会和执行线程打架。
  const canRetry = Boolean(onRetryStep) && entry.status === 'failed'
    && Boolean(entry.workflowRunId) && Boolean(entry.failedStepId);
  async function perform(action: 'cancel' | 'approve' | 'reject' | 'pause' | 'resume' | 'retry') {
    if (busy) return;
    setBusy(action);
    setActionError('');
    try {
      if (action === 'cancel') await onCancelRun?.(entry.workflowRunId);
      if (action === 'approve') await onApproveRun?.(entry.workflowRunId, entry.currentStepId);
      if (action === 'reject') await onRejectRun?.(entry.workflowRunId, entry.currentStepId);
      if (action === 'pause') await onPauseRun?.(entry.workflowRunId);
      if (action === 'resume') await onResumePausedRun?.(entry.workflowRunId);
      if (action === 'retry') await onRetryStep?.(entry.workflowRunId, entry.failedStepId);
    } catch (error) {
      const label = action === 'cancel' ? '取消失败' : action === 'pause' ? '暂停失败'
        : action === 'resume' ? '恢复失败' : action === 'retry' ? '重试失败' : '审批失败';
      setActionError(formatError(error, label));
    } finally {
      setBusy('');
    }
  }
  return (
    <article className={`task-center-row${featured ? ' featured' : ''}${active ? ' is-live' : ''}`}>
      <span className={`task-center-state-icon ${tone}${spinning ? ' is-spinning' : ''}`}>
        <MorphIcon icon={StatusIcon} size={16} strokeWidth={2} />
      </span>
      <div className="task-center-row-main">
        <div className="task-center-row-title">
          <strong>{entry.title}</strong>
          {/* 角色徽标只在工作流声明了角色时出现：没有角色的运行不该凭空多一个「未分配」。 */}
          {shouldShowRoleBadge(entry.roleId) ? <span className="task-center-role" title={entry.roleId}>{roleLabel(entry.roleId)}</span> : null}
          {parentKind ? <span className={`task-center-parent ${entry.parentKind}`} title={entry.parentKind}>{parentKind}</span> : null}
          <span className={`task-center-source ${entry.source}`}>{entry.sourceLabel}</span>
          <span className={`task-center-status ${tone}`}>{taskStatusLabel(entry.status)}</span>
        </div>
        <div className="task-center-row-meta">
          <code title={entry.id}>{shortRunId(entry.id)}</code>
          <time title={formatAbsoluteTime(activityTime(entry))}>{formatRelativeTime(activityTime(entry), now)}</time>
          {elapsed !== null ? <span className="task-center-elapsed run-live-label">已运行 {formatElapsedCn(elapsed)}</span> : duration ? <span>耗时 {duration}</span> : null}
          {stepLabel ? <span>{stepLabel}</span> : null}
          {/* 协作规模：委派和重做分开报，否则「多角色分工」会被读成「返工」。
              用独立节点而不是拼一串文字，分隔线由样式统一画，窄窗也不会粘在一起。 */}
          {team ? (
            <span className="task-center-team" aria-label="协作规模">
              {team.delegated > 0 ? <span><b>{team.delegated}</b> 项委派</span> : null}
              {team.retries > 0 ? <span><b>{team.retries}</b> 次重做</span> : null}
              {team.running > 0 ? <span className="is-live"><b>{team.running}</b> 项进行中</span> : null}
            </span>
          ) : null}
          {entry.subtitle ? <span>{entry.subtitle}</span> : null}
          {entry.workflowRunId && onOpenWorkflowRun ? <button type="button" className="task-center-row-action" onClick={() => onOpenWorkflowRun(entry.workflowRunId)}>查看运行</button> : null}
          {/* 行内操作：看见任务的地方就该能处理它，不必跳页。
              只在动作真的可用时出现，不做灰按钮。 */}
          {canDecide ? (
            <>
              <button type="button" className="task-center-row-action is-primary" disabled={Boolean(busy)} onClick={() => void perform('approve')}>
                {busy === 'approve' ? '处理中' : '批准'}
              </button>
              <button type="button" className="task-center-row-action is-danger" disabled={Boolean(busy)} onClick={() => void perform('reject')}>
                {busy === 'reject' ? '处理中' : '拒绝'}
              </button>
            </>
          ) : null}
          {canPause ? (
            <button type="button" className="task-center-row-action" disabled={Boolean(busy)} onClick={() => void perform('pause')}>
              {busy === 'pause' ? '暂停中' : '暂停'}
            </button>
          ) : null}
          {canResume ? (
            <button type="button" className="task-center-row-action is-primary" disabled={Boolean(busy)} onClick={() => void perform('resume')}>
              {busy === 'resume' ? '恢复中' : '继续'}
            </button>
          ) : null}
          {canRetry ? (
            <button type="button" className="task-center-row-action is-primary" disabled={Boolean(busy)} onClick={() => void perform('retry')}>
              {busy === 'retry' ? '重试中' : '重试'}
            </button>
          ) : null}
          {canCancel ? (
            <button type="button" className="task-center-row-action is-danger" disabled={Boolean(busy)} onClick={() => void perform('cancel')}>
              {busy === 'cancel' ? '取消中' : '取消'}
            </button>
          ) : null}
        </div>
        {actionError ? <p className="error" role="alert">{actionError}</p> : null}
        {/* 协作链路只在真的有两个以上角色时出现：一个角色不叫协作，画成链路只会占地方。 */}
        {shouldShowRoleChain(entry.roles) ? (
          <div className="task-center-role-chain" role="list" aria-label="角色协作顺序">
            {entry.roles.map((role, index) => {
              const state = chainState(role.status);
              const last = index === entry.roles.length - 1;
              return (
                <span
                  className={`task-center-role-step is-${state}${last ? ' is-last' : ''}`}
                  role="listitem"
                  key={role.id}
                  title={`${role.id} · ${taskStatusLabel(role.status) || '未开始'}`}
                >
                  <span className="task-center-role-dot" aria-hidden="true" />
                  <span className="task-center-role-name">{role.label}</span>
                </span>
              );
            })}
          </div>
        ) : null}
        {/* 子运行明细：只在真有子运行时给展开入口，否则不留一个空三角。 */}
        {entry.childrenRuns.length > 0 ? (
          <details className="task-center-children">
            <summary>
              <span>子运行 {entry.childrenRuns.length} 条</span>
            </summary>
            <div className="task-center-children-list" role="list">
              {entry.childrenRuns.map(child => (
                <div className="task-center-child" role="listitem" key={child.runId}>
                  <span className={`task-center-child-kind ${child.kind}`}>{child.kindLabel || child.kind || '子运行'}</span>
                  {child.roleLabel ? <span className="task-center-child-role">{child.roleLabel}</span> : null}
                  <span className={`task-center-child-status ${taskStatusTone(child.status)}`}>{child.statusLabel || child.status}</span>
                  <code title={child.runId}>{shortRunId(child.runId)}</code>
                  {child.error ? <span className="task-center-child-error" title={child.error}>{child.error}</span> : null}
                  {child.updatedAt ? <time title={formatAbsoluteTime(child.updatedAt)}>{formatRelativeTime(child.updatedAt, now)}</time> : null}
                </div>
              ))}
            </div>
          </details>
        ) : null}
        {message ? <p className={entry.error ? 'error' : ''}>{message}</p> : null}
        {active ? (
          progress === null
            ? <div className="task-center-progress is-indeterminate" role="progressbar" aria-label={`${entry.title}进度`} aria-valuetext="进行中"><span /></div>
            : <div className="task-center-progress" role="progressbar" aria-label={`${entry.title}进度`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={progress}><span style={{ width: `${progress}%` }} /></div>
        ) : null}
      </div>
      {active ? <strong className={`task-center-progress-label${progress === null ? ' is-live' : ''}`}>{taskProgressLabel(entry.progress)}</strong> : null}
    </article>
  );
}
