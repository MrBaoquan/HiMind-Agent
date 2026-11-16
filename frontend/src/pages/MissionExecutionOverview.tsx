import type { AgentActivityItem, MissionEvent, MissionTaskView, MissionView } from '../services/agentApi';
import { BusyIndicator } from '../components/BusyIndicator';
import { eventLabel, missionNextAction, missionResponsibility, missionResumeAction, missionRunningTasks, missionRunningVerifications, missionSchedulingWait, missionTaskActivity, missionTaskExecutionLabel, missionWait, taskFailureReason, taskTitle } from './missionView';

const normalizeEvent = (event: MissionEvent) => event.event_type.replace(/\./g, '_');

export function executionEventLabel(event: MissionEvent) {
  const labels: Record<string, string> = {
    task_workspace_prepared: '隔离工作区已准备',
    task_candidate_integrated: '交付候选已整合，待检查与核验',
    parallel_wave_admitted: '已安排一批任务',
    parallel_wave_waiting: '调度等待依赖、名额或资源释放',
  };
  if (normalizeEvent(event).includes('serial_fallback')) return '本次改为串行执行';
  const label = eventLabel(event);
  return labels[normalizeEvent(event)] || (label === event.event_type ? event.summary || '任务状态更新' : label);
}

export function missionWaitingReason(mission: MissionView) {
  if (mission.status !== 'waiting') return '';
  const wait = missionWait(mission);
  if (!wait) return '等待原因未确认，请刷新状态';
  if (wait.budget_reason) return wait.budget_reason;
  return ({ budget_exhausted: '预算已用尽', environment_blocked: '命令环境需要处理', manual_pause: '已暂停后续派发',
    system_verification: '等待系统检查或核验', verification_resume_required: '等待手动续跑核验' } as Record<string, string>)[wait.reason]
    || '等待原因尚未确认，请刷新状态';
}

function latestEvent(events: MissionEvent[]) {
  return events.reduce<MissionEvent | undefined>((latest, event) => !latest || event.sequence > latest.sequence ? event : latest, undefined);
}

function RecentEvent({ event }: { event?: MissionEvent }) {
  return event ? <>{executionEventLabel(event)}{event.summary && event.summary !== executionEventLabel(event) ? `：${event.summary}` : ''}
    {' · '}<time dateTime={event.occurred_at}>{new Date(event.occurred_at).toLocaleString()}</time></> : <>尚无任务活动记录</>;
}

function taskSituation(mission: MissionView, task: MissionTaskView, verificationLive: boolean, activities: AgentActivityItem[]) {
  const paused = mission.status === 'waiting';
  const pendingDependencies = task.spec.depends_on.filter(id => {
    const dependency = mission.tasks.find(value => value.spec.id === id);
    return !dependency || dependency.status !== 'completed' || dependency.verification_passed !== true;
  });
  const dependencyReason = pendingDependencies.length ? `等待依赖通过验收：${pendingDependencies.map(id => taskTitle(mission, id)).join('、')}` : '';
  switch (task.status) {
    case 'running': {
      const activity = missionTaskActivity(mission, task, activities);
      return { status: missionTaskExecutionLabel(mission, task, activities),
        wait: activity?.status === 'running' ? '无' : activity?.status === 'waiting' ? '执行器等待恢复或外部条件'
          : activity?.status === 'queued' ? '执行器排队中，等待开始运行' : '尚未收到当前执行的运行状态',
        next: activity?.status === 'waiting' ? '查看关联执行的等待原因，条件满足后继续'
          : paused ? '等待当前执行回报；后续派发已暂停' : '等待执行器回报状态与结果' };
    }
    case 'awaiting_verification': return {
      status: verificationLive ? '独立核验进行中' : '等待核验',
      wait: verificationLive ? '无' : paused ? missionWaitingReason(mission) : task.verification_run_id ? '已派发核验，等待运行状态更新' : '等待系统检查与独立核验',
      next: verificationLive ? '等待独立核验结果' : missionResumeAction(mission)?.kind === 'verification' ? '手动续跑核验' : paused ? '按当前等待原因处理，保留已提交实现' : '由系统检查并安排独立核验',
    };
    case 'blocked': return { status: '等待依赖', wait: dependencyReason || '阻塞原因尚未上报', next: '等待依赖通过验收后由系统重新调度' };
    case 'ready': return { status: '待派发', wait: paused ? missionWaitingReason(mission) : dependencyReason || '等待系统派发', next: paused ? '按当前等待原因处理后继续' : '由系统核对并发额度与资源后派发' };
    case 'completed': return { status: '已完成', wait: '无', next: '查看结果与验收证据' };
    case 'failed': return { status: '已失败', wait: '执行或核验未通过', next: '查看失败原因与验收证据' };
    case 'canceled': return { status: '已取消或未纳入', wait: '无', next: '保留记录，不再派发' };
    default: return { status: '待编排', wait: '等待计划确认', next: '审查计划与访问范围' };
  }
}

function serialRecord(event: MissionEvent, mission: MissionView) {
  if (event.payload.plan_sha256 && event.payload.plan_sha256 !== mission.plan_sha256) return '';
  if (normalizeEvent(event).includes('serial_fallback')) {
    return typeof event.payload.reason === 'string' ? event.payload.reason : event.summary;
  }
  if (normalizeEvent(event) !== 'parallel_wave_admitted') return '';
  const reasons = Array.isArray(event.payload.serial_fallbacks) ? event.payload.serial_fallbacks.flatMap(value => {
    if (!value || typeof value !== 'object' || typeof value.reason !== 'string') return [];
    return [`${typeof value.task_id === 'string' ? `${taskTitle(mission, value.task_id)}：` : ''}${value.reason}`];
  }) : [];
  if (event.payload.budget_serial === true) reasons.push('本批次受预算约束，按串行推进');
  return reasons.join('；');
}

/** Store task states establish occupied slots; current ledger activities alone
 * establish live execution. Admission/preparation events never imply activity. */
export function MissionExecutionOverview({ mission, events, activities = [] }: {
  mission: MissionView; events: MissionEvent[]; activities?: AgentActivityItem[];
}) {
  const assigned = mission.tasks.filter(task => task.status === 'running');
  const running = missionRunningTasks(mission, activities);
  const verifications = missionRunningVerifications(mission, activities);
  const recent = latestEvent(events.filter(event => event.mission_id === mission.mission_id));
  const serial = latestEvent(events.filter(event => event.mission_id === mission.mission_id && serialRecord(event, mission)));
  const executorLabels: Record<string, string> = { 'himind.builtin': 'HiMind AI', 'personal.codex': 'Codex CLI', 'personal.github-copilot': 'Copilot CLI' };
  return <section className="mission-detail-panel" aria-label="执行现场">
    <div className="mission-detail-panel-head"><h3>执行现场</h3><strong aria-label="同时执行任务数" aria-live="polite">{running.length} 项任务同时执行</strong></div>
    <p><span aria-label="已派发占用任务数">{assigned.length} 项已派发占用</span> · {verifications.length} 项独立核验运行中{mission.scheduling ? ` · 配置并发上限 ${mission.scheduling.max_active_tasks}` : ' · 未配置并行调度'}</p>
    <p>当前责任者：{missionResponsibility(mission, activities)}</p>
    {missionWaitingReason(mission) ? <p>等待原因：{missionWaitingReason(mission)}</p> : null}
    <p>最近活动：<RecentEvent event={recent}/></p>
    {serial ? <p>最近串行降级记录：{serialRecord(serial, mission)}{' · '}<time dateTime={serial.occurred_at}>{new Date(serial.occurred_at).toLocaleString()}</time></p> : null}
    {mission.scheduling && (Object.keys(mission.scheduling.executor_limits || {}).length || Object.keys(mission.scheduling.task_resources || {}).length) ? <details>
      <summary>并发与资源配置</summary>
      {Object.entries(mission.scheduling.executor_limits || {}).map(([executor, limit]) => <p key={executor}>{executorLabels[executor] || executor}：最多 {limit} 项</p>)}
      {Object.entries(mission.scheduling.task_resources || {}).map(([id, resources]) => <p key={id}>{taskTitle(mission, id)} · 独占资源：{resources.join('、') || '无'}</p>)}
    </details> : null}
    {mission.tasks.length ? <div className="mission-task-list">{mission.tasks.map(task => {
      const verificationLive = verifications.some(value => value.spec.id === task.spec.id);
      const situation = taskSituation(mission, task, verificationLive, activities);
      const role = mission.assignment?.roles.find(value => value.role_id === task.spec.execution.role_id);
      const executor = executorLabels[task.spec.execution.executor] || task.spec.execution.executor;
      const owner = task.status === 'awaiting_verification' ? '系统与独立核验专家' : `${role?.label || task.spec.execution.role_id || '未指派职责'}${executor ? ` · ${executor}` : ''}`;
      const taskEvent = latestEvent(events.filter(event => event.mission_id === mission.mission_id && event.task_id === task.spec.id));
      const schedulingReason = ['ready', 'blocked'].includes(task.status) ? missionSchedulingWait(mission, task.spec.id, events) : '';
      const live = running.some(value => value.spec.id === task.spec.id) || verificationLive;
      return <article className={`mission-task-row task-${task.status}`} aria-label={task.spec.title} key={task.spec.id} data-execution-task-id={task.spec.id} data-task-status={task.status} data-running={live}>
        <span className="mission-task-status">{live ? <BusyIndicator size={12}/> : null}{situation.status}</span>
        <div><strong>{task.spec.title}</strong><dl className="task-facts">
          <div><dt>责任者</dt><dd>{owner}</dd></div>
          <div><dt>等待原因</dt><dd>{task.status === 'failed' ? taskFailureReason(events, task.spec.id) || situation.wait : situation.wait}{schedulingReason ? <small>最近调度记录：{schedulingReason}</small> : null}</dd></div>
          <div><dt>最近活动</dt><dd><RecentEvent event={taskEvent}/></dd></div>
          <div><dt>下一步</dt><dd>{situation.next}</dd></div>
        </dl></div>
      </article>;
    })}</div> : <p>尚无实际执行任务；确认计划后显示任务状态。</p>}
    <p>下一步：{missionNextAction(mission, events, activities)}</p>
  </section>;
}
