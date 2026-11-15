import { useCallback, useEffect, useMemo, useState, useRef, useLayoutEffect } from 'react';
import { Activity, AlertCircle, Search } from 'lucide-react';
// 状态图标链吃的是图标数据：同一个图标位在等待/运行/完成之间形变。
import { CircleAlert, CircleCheck, CircleDashed, Clock, LoaderCircle as loaderIconData } from 'lucide';
import { BusyIndicator } from '../components/BusyIndicator';
import { EmptyState, PageHeader } from '../components/Common';
import { MorphIcon } from '../components/MorphIcon';
import type { AgentActivityItem, AgentTaskHistoryItem, CurrentTaskStatus } from '../services/agentApi';
import {
  TASK_ACTIVE_STATUSES,
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
import { CollaborationTaskDetail, CreateMissionDialog } from './MissionTeamPanel';
import { useMissionCatalog, type MissionCatalog } from './useMissionCatalog';
import { executionTodoId, activityNeedsUser, activityStatusLabel, projectTasks, missionTodo, workspaceKey, EMPTY_TASK_CONTEXT, type TaskListContext } from './taskCenterModel';
import { missionActionActor, missionWait, missionResponsibility, missionNextAction, missionProgress, missionStage, missionStatusLabel, MISSION_STAGE_LABELS } from './missionView';

type ActivitySource = 'dashboard' | 'workflow' | 'skill' | 'schedule';

type TaskCenterPageProps = {
  currentTask: CurrentTaskStatus | null;
  missionCatalog?: MissionCatalog;
  initialContext?: TaskListContext;
  onContextChange?: (context: TaskListContext) => void;
  onMissionSelectionChange?: (missionId: string) => void;
  onOpenWorkflowLibrary?: () => void;
  dashboardEnabled: boolean;
  /** Mission deep links select one collaboration task on first render. */
  initialMissionId?: string;
  /** A new revision makes repeated navigation to the same Mission observable. */
  missionNavigationRevision?: number;
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

const SOURCE_LABELS: Record<ActivitySource, string> = {
  dashboard: '工作台任务',
  workflow: '工作流运行',
  skill: '技能运行',
  schedule: '技能运行',
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

export function TaskCenterPage({ currentTask, dashboardEnabled, initialMissionId, missionNavigationRevision,
  missionCatalog, initialContext = EMPTY_TASK_CONTEXT, onContextChange, onMissionSelectionChange, onOpenWorkflowLibrary,
  onLoadTaskHistory, onLoadLocalActivity, onOpenDashboard, onOpenWorkflowRun, ...actions }: TaskCenterPageProps) {
  const fallbackCatalog = useMissionCatalog(!missionCatalog);
  const catalog = missionCatalog || fallbackCatalog;
  const [items, setItems] = useState<AgentActivityItem[]>([]);
  const [context, setContext] = useState(initialContext);
  const contextRef = useRef(context); contextRef.current = context;
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [missionId, setMissionId] = useState(initialMissionId || '');
  const [activityId, setActivityId] = useState('');
  const detailRef = useRef(false); detailRef.current = Boolean(missionId || activityId);
  const [createOpen, setCreateOpen] = useState(false);
  const [newMenu, setNewMenu] = useState(false);
  const restoreScroll = useRef(true);
  const main = () => document.querySelector<HTMLElement>('.main-content');
  const rememberScroll = () => {
    const next = { ...contextRef.current, scrollTop: detailRef.current ? contextRef.current.scrollTop : main()?.scrollTop ?? window.scrollY };
    contextRef.current = next; setContext(next); onContextChange?.(next);
  };
  const openMission = (id: string) => {
    if (id) rememberScroll(); else restoreScroll.current = true;
    setMissionId(id); onMissionSelectionChange?.(id);
  };
  useEffect(() => { setMissionId(initialMissionId || ''); }, [initialMissionId, missionNavigationRevision]);
  useEffect(() => { onContextChange?.(context); }, [context, onContextChange]);
  useEffect(() => {
    const host = main() || window;
    const save = () => { if (detailRef.current) return; contextRef.current = { ...contextRef.current, scrollTop: main()?.scrollTop ?? window.scrollY }; };
    host.addEventListener('scroll', save, {passive:true});
    return () => { host.removeEventListener('scroll', save); onContextChange?.(contextRef.current); };
  }, [onContextChange]);
  const load = useCallback(async (silent = false, includeRemote = true) => {
    if (!silent) setLoading(true);
    const [local, remote] = await Promise.allSettled([
      onLoadLocalActivity(),
      dashboardEnabled && includeRemote ? onLoadTaskHistory() : Promise.resolve([] as AgentTaskHistoryItem[]),
    ]);
    setItems(current => {
      const localItems = local.status === 'fulfilled' ? local.value : current.filter(i => i.source !== 'dashboard');
      const remoteItems = includeRemote && remote.status === 'fulfilled'
        ? remote.value.map(i => ({ ...i, source:'dashboard', title:taskTypeLabel(i.task_type), progress:i.progress ?? null }))
        : current.filter(i => i.source === 'dashboard');
      return [...localItems, ...remoteItems];
    });
    setError(local.status === 'rejected' || remote.status === 'rejected' ? '部分记录读取失败，保留上次结果。' : '');
    setLoading(false);
  }, [dashboardEnabled, onLoadLocalActivity, onLoadTaskHistory]);
  useEffect(() => {
    void load(); let tick = 0;
    const timer = window.setInterval(() => { if (document.visibilityState !== 'hidden') void load(true, ++tick % 6 === 0); }, 5000);
    return () => window.clearInterval(timer);
  }, [load]);
  const {rows, related} = useMemo(() => projectTasks(catalog.missions, items, catalog.activities || items), [catalog.missions, catalog.activities, items]);
  const missionActivities = (id: string) => [...new Map([
    ...(related.get('mission:'+id) || []), ...(catalog.activities || []).filter(item => item.mission_id === id),
  ].map(item => [item.workflow_run_id || item.id, item])).values()];
  const scoped = rows.filter(row => (!context.workspace || (context.workspace === '__unknown' ? !row.workspace : workspaceKey(row.workspace) === context.workspace))
    && (!context.method || row.method === context.method)
    && (!context.query.trim() || (row.search + ' ' + row.client).toLowerCase().includes(context.query.trim().toLowerCase())));
  const counts = {all:scoped.length, attention:scoped.filter(r=>r.needsUser).length, running:scoped.filter(r=>!r.ended).length, ended:scoped.filter(r=>r.ended).length};
  const filtered = scoped.filter(row => context.view === 'all' || (context.view === 'attention' ? row.needsUser : context.view === 'ended' ? row.ended : !row.ended));
  const workspaces = new Map(rows.filter(r=>r.workspace).map(r=>[workspaceKey(r.workspace), r.workspace]));
  const update = (patch: Partial<TaskListContext>) => setContext(current => ({...current, ...patch, scrollTop:0}));
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    if (!rows.some(row => !row.ended)) return;
    const timer=window.setInterval(()=>setNow(Date.now()),1000); return ()=>window.clearInterval(timer);
  }, [rows.some(row => !row.ended)]);
  useLayoutEffect(() => {
    if (missionId || activityId || loading || catalog.loading || !restoreScroll.current) return;
    const frame = requestAnimationFrame(() => {
      const host = main(); if (host) host.scrollTop = contextRef.current.scrollTop; else window.scrollTo(0, contextRef.current.scrollTop);
      restoreScroll.current = false;
    });
    return () => cancelAnimationFrame(frame);
  }, [missionId, activityId, loading, catalog.loading]);
  const openRun = onOpenWorkflowRun ? (id:string) => { rememberScroll(); onOpenWorkflowRun(id); } : undefined;
  const selectedActivity = items.find(i=>i.id===activityId);
  return <div className="task-center-page">
    <PageHeader title="我的任务" description="跟踪任务进展、处理待办、查看交付结果。" actions={!missionId && !activityId ? <div className="task-create-menu">
      <button type="button" className="btn btn-primary" aria-expanded={newMenu} onClick={()=>setNewMenu(v=>!v)}>新建任务</button>
      {newMenu ? <div className="task-create-options"><button className="btn" onClick={()=>{setNewMenu(false);setCreateOpen(true);}}>新建协作任务</button>{onOpenWorkflowLibrary ? <button className="btn" onClick={()=>{setNewMenu(false);onOpenWorkflowLibrary();}}>运行工作流</button> : null}{dashboardEnabled ? <button className="btn" onClick={onOpenDashboard}>打开工作台</button> : null}</div> : null}
    </div> : undefined} />
    {missionId ? <CollaborationTaskDetail key={missionId} missionId={missionId} openAssignmentRevision={missionNavigationRevision}
      onBack={()=>openMission('')} onRefresh={catalog.refresh} onMissionChanged={catalog.upsert}
      relatedActivities={missionActivities(missionId)} onOpenWorkflowRun={openRun} />
    : selectedActivity ? <section className="mission-detail">
      <button className="btn" onClick={()=>{restoreScroll.current=true;setActivityId('');}}>返回我的任务</button>
      <header className="mission-detail-hero"><div><h2>{selectedActivity.title}</h2><p>{selectedActivity.subtitle || '执行记录'}</p></div><strong>{activityStatusLabel(selectedActivity) || taskStatusLabel(selectedActivity.status)}</strong></header>
      <dl className="task-facts"><div><dt>工作区</dt><dd>{selectedActivity.workspace_root || '未记录'}</dd></div><div><dt>来源</dt><dd>{selectedActivity.source_client || (selectedActivity.source==='dashboard'?'工作台':'未记录')}</dd></div><div><dt>更新时间</dt><dd>{formatAbsoluteTime(selectedActivity.updated_at)}</dd></div></dl>
      <section className="mission-detail-panel"><h3>概览</h3><p>{selectedActivity.error || selectedActivity.detail || '尚无进一步执行说明'}</p>{selectedActivity.output_path ? <><h3>交付物</h3><code className="task-evidence-path">{selectedActivity.output_path}</code></> : <p className="muted">未记录可展示的交付文件。</p>}</section>
    </section> : <>
      <section className="task-center-history" aria-label="任务列表">
        <div className="task-center-toolbar">
          <label className="task-center-search"><Search size={15}/><span className="sr-only">搜索任务</span><input value={context.query} onChange={e=>update({query:e.target.value})} placeholder="搜索任务、目标、工作区或编号"/></label>
          <label className="task-select-filter"><span>工作区</span><select aria-label="工作区筛选" value={context.workspace} onChange={e=>update({workspace:e.target.value})}><option value="">全部工作区</option>{[...workspaces].map(([key,value])=><option key={key} value={key}>{value}</option>)}<option value="__unknown">未记录工作区</option></select></label>
          <details className="task-more-filters"><summary>更多筛选</summary><label>执行方式<select aria-label="执行方式筛选" value={context.method} onChange={e=>update({method:e.target.value})}><option value="">全部执行方式</option>{[...new Set(rows.map(r=>r.method))].map(v=><option key={v}>{v}</option>)}</select></label></details>
        </div>
        <div className="task-center-filters" role="tablist" aria-label="任务视图">{([{id:'all',label:'全部'},{id:'attention',label:'待我处理'},{id:'running',label:'进行中'},{id:'ended',label:'已结束'}] as const).map(v=><button key={v.id} role="tab" aria-selected={context.view===v.id} className={context.view===v.id?'active':''} onClick={()=>update({view:v.id})}>{v.label}<small>{counts[v.id]}</small></button>)}</div>
        <p className="task-list-scope">显示最近读取的 {rows.length} 项顶层任务；待我处理表示需要你的行动，可与进行中重叠。</p>
        {error || catalog.error ? <div className="task-center-alert" role="alert"><AlertCircle size={16}/><span>{error || catalog.error}</span><button className="btn" onClick={()=>{void load();void catalog.refresh();}}>重新读取</button></div> : null}
        {(loading || catalog.loading) && !rows.length ? <div className="task-center-loading"><BusyIndicator/>正在读取任务</div>
          : filtered.length ? <div className="task-center-list">{filtered.map(row=>{
            if (row.mission) {
              const mission=row.mission; const progress=missionProgress(mission); const activities=missionActivities(mission.mission_id); const todo=missionTodo(mission, activities); const wait=missionWait(mission);
              return <article className="task-center-row mission-task-entry" key={row.id} data-task-id={row.id} data-todo-id={todo?.id} data-snapshot-cursor={wait?.cursor} data-wait-reason={wait?.reason} data-wait-actor={missionActionActor(mission, activities)}>
                <div className="task-center-row-main"><div className="task-center-row-title"><button type="button" className="task-title-link" onClick={()=>openMission(mission.mission_id)}>{row.title}</button><span className="task-center-source">协作任务</span><span className={'collaboration-task-status status-'+mission.status}>{missionStatusLabel(mission, [], activities)}</span></div>
                <div className="task-center-row-meta"><span title={row.workspace}>{row.workspace || '未记录工作区'}</span><span>来源：{row.client}</span>{row.trigger?<span>{row.trigger}</span>:null}<time>{formatRelativeTime(row.updatedAt,now)}</time></div>
                <p className="task-row-next">{missionNextAction(mission, [], activities)}</p><p className="task-row-next muted">{missionResponsibility(mission, activities)}</p><div className="task-center-row-meta"><span>阶段：{MISSION_STAGE_LABELS[missionStage(mission)]}</span>{progress.total?<span>{progress.done}/{progress.total} 项原始交付已通过验收 · {progress.awaitingVerification} 项待验收</span>:<span>未记录执行进度</span>}<button type="button" className="task-center-row-action" onClick={()=>openMission(mission.mission_id)}>{todo ? todo.action : '查看任务'}</button></div></div>
              </article>;
            }
            const item=row.activity!; const entry=item.source==='dashboard'?fromTask(item as unknown as AgentTaskHistoryItem):fromActivity(item);
            const children=related.get(row.id)||[];
            if (children.length) entry.childrenRuns = entry.childrenRuns.filter(child=>!children.some(item=>item.workflow_run_id===child.runId));
            return <div key={row.id} data-task-id={row.id} data-todo-id={activityNeedsUser(item) ? executionTodoId(item.workflow_run_id || item.id, item.current_step_id || '', item.pause_kind || '') : undefined} className="task-unified-entry"><ActivityRow entry={entry} now={now} featured={item.id===currentTask?.task_id} onOpenWorkflowRun={openRun} {...actions}/>
              <div className="task-entry-context"><span title={row.workspace}>{row.workspace || '未记录工作区'}</span><span>来源：{row.client}</span><span>{row.method}{row.trigger?' · '+row.trigger:''}</span>{!item.workflow_run_id?<button className="task-center-row-action" onClick={()=>{rememberScroll();setActivityId(item.id);}}>查看任务</button>:null}</div>
              {children.length ? <details className="task-related-runs"><summary>{children.length} 次关联执行（委派 / 重试）</summary>{children.map(child=><div key={child.id}><span>{child.title} · {activityStatusLabel(child)||taskStatusLabel(child.status)}</span>{openRun&&child.workflow_run_id?<button className="task-center-row-action" onClick={()=>openRun(child.workflow_run_id!)}>查看执行</button>:null}</div>)}</details>:null}
            </div>;
          })}</div> : <EmptyState icon={Activity} title={rows.length?'没有匹配的任务':'还没有任务'} text={rows.length?'调整搜索或筛选条件。':'新建协作任务或运行工作流后，在这里跟踪进展。'}/>}
      </section>
    </>}
    {createOpen ? <CreateMissionDialog onClose={()=>setCreateOpen(false)} onCreated={mission=>{catalog.upsert(mission);setCreateOpen(false);openMission(mission.mission_id);void catalog.refresh();}}/>:null}
  </div>;
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
  const awaitingApproval = entry.status === 'waiting' && entry.pauseKind === 'approval' && Boolean(entry.currentStepId);
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
          <span className={`task-center-status ${tone}`}>{activityStatusLabel({status:entry.status,pause_kind:entry.pauseKind}) || taskStatusLabel(entry.status)}</span>
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
