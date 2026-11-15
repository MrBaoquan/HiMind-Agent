import type { MissionEvent, MissionStatus, MissionStatus as MissionStatusType, MissionTaskStatus, MissionView } from '../services/agentApi';

export const MISSION_STATUS_LABELS: Record<MissionStatus, string> = {
  draft: '待规划',
  awaiting_assignment: '待指派',
  planned: '待生成计划',
  awaiting_confirmation: '待确认',
  active: '执行中',
  waiting: '暂停/等待',
  completed: '已完成',
  failed: '已失败',
  canceled: '已取消',
};

export const MISSION_TASK_STATUS_LABELS: Record<MissionTaskStatus, string> = {
  proposed: '待编排',
  blocked: '等待依赖',
  ready: '可派发',
  running: '执行中',
  awaiting_verification: '待另一位专家核验',
  failed: '失败',
  completed: '已完成',
  canceled: '未选中',
};

export const MISSION_FLOW_STEPS = ['指派专家', '确认计划', '执行任务', '独立验收', '交付完成'] as const;

export function missionStalled(mission: MissionView) {
  return mission.status === 'active' && mission.tasks.some(task => task.status === 'failed')
    && !mission.tasks.some(task => ['ready', 'running', 'awaiting_verification'].includes(task.status));
}

export function taskFailureReason(events: MissionEvent[], taskId: string) {
  const event = [...events].reverse().find(item => item.task_id === taskId
    && ['verification_failed', 'task_failed'].includes(item.event_type));
  return typeof event?.payload?.reason === 'string' ? event.payload.reason : event?.summary || '';
}

export function missionStage(mission: MissionView): keyof typeof MISSION_STAGE_LABELS {
  if (['completed', 'failed', 'canceled'].includes(mission.status)) return mission.status as 'completed' | 'failed' | 'canceled';
  if (mission.status === 'waiting') return 'waiting';
  if (mission.status === 'awaiting_assignment') return 'assignment';
  if (mission.status === 'draft' || mission.status === 'planned') return 'plan';
  if (mission.status === 'awaiting_confirmation') return 'confirm';
  if (missionStalled(mission)) return 'attention';
  const pending = mission.tasks.filter(task => !['completed', 'canceled'].includes(task.status));
  if (pending.some(task => task.status === 'awaiting_verification') && pending.every(task => ['awaiting_verification', 'blocked'].includes(task.status))) return 'verify';
  return 'execute';
}
export const MISSION_STAGE_LABELS = {
  assignment: '配置专家', plan: '制定计划', confirm: '审查计划', execute: '执行子任务',
  verify: '独立验收', attention: '需要处理', waiting: '暂停或等待', completed: '已完成', failed: '执行失败', canceled: '已取消',
} as const;

export function missionFlow(mission: MissionView) {
  const stage = missionStage(mission);
  const verificationIsPending = mission.tasks.some(task => task.status === 'awaiting_verification');
  const currentIndex = stage === 'assignment' ? 0
    : stage === 'plan' || stage === 'confirm' ? 1
      : stage === 'verify' || ((stage === 'waiting' || stage === 'failed' || stage === 'attention') && (verificationIsPending || mission.tasks.some(task => task.verification_passed === false))) ? 3
        : stage === 'completed' ? 4
          : 2;
  return MISSION_FLOW_STEPS.map((label, index) => ({
    label,
    state: mission.status === 'completed' || index < currentIndex ? 'completed' as const
      : index === currentIndex ? 'current' as const : 'upcoming' as const,
  }));
}

export function missionProgress(mission: MissionView) {
  const selected = mission.tasks.filter(task => task.status !== 'canceled');
  const done = selected.filter(task => task.status === 'completed').length;
  return {
    done,
    total: selected.length,
    running: selected.filter(task => task.status === 'running').length,
    awaitingVerification: selected.filter(task => task.status === 'awaiting_verification').length,
    startedVerification: selected.filter(task => task.status === 'awaiting_verification' && Boolean(task.verification_run_id)).length,
    ready: selected.filter(task => task.status === 'ready').length,
    blocked: selected.filter(task => task.status === 'blocked').length,
    failed: selected.filter(task => task.status === 'failed').length,
  };
}

/** Environment state is independent of later diagnostic/progress events. */
export function missionRuntimeBlocked(mission: MissionView, events: MissionEvent[]) {
  if (mission.status !== 'waiting') return false;
  const last = [...events].reverse().find(event => ['runtime_workspace_blocked', 'runtime_workspace_resolved'].includes(event.event_type));
  return last?.event_type === 'runtime_workspace_blocked' && (!last.payload?.plan_sha256 || last.payload.plan_sha256 === mission.plan_sha256);
}

export function missionStatusLabel(mission: MissionView, events: MissionEvent[] = []) {
  if (mission.status === 'active') {
    if (missionStalled(mission)) return '任务已停下，需处理失败';
    const latest = events[events.length - 1];
    if (latest?.event_type === 'mission_supervisor_error') return '暂时无法推进';
    if (latest?.event_type === 'verification_preparing') return '系统检查中';
    const progress = missionProgress(mission);
    if (progress.awaitingVerification && !progress.running) return progress.startedVerification ? '核验进行中' : '待核验';
  }
  if (mission.status === 'waiting') {
    if (missionRuntimeBlocked(mission, events)) return '命令环境待处理';
    const transition = events[events.length - 1];
    if (transition?.event_type === 'budget_exhausted') return '预算已用尽';
    if (transition?.event_type === 'mission_pause') return '已暂停';
    if (transition?.event_type === 'mission_waiting') return '等待处理';
    if (transition?.event_type === 'mission_follow_up_waiting') return '返工计划待恢复';
    if (transition?.event_type === 'verification_runtime_failed') return '核验运行中断';
  }
  return MISSION_STATUS_LABELS[mission.status];
}

export function missionVerificationExplanation(mission: MissionView, events: MissionEvent[] = []) {
  const progress = missionProgress(mission);
  if (!progress.awaitingVerification) return '';
  const latest = events[events.length - 1];
  if (mission.status === 'active' && latest?.event_type === 'mission_supervisor_error') return latest.summary;
  if (mission.status === 'active' && latest?.event_type === 'verification_preparing') return '系统正在检查任务文件并运行测试，随后由另一位专家核对结果。此时无需你操作。';
  if (progress.startedVerification === progress.awaitingVerification) {
    return '实现专家已提交结果。系统正在运行必需检查，并由另一位专家核对交付内容；通过后子任务才算完成。';
  }
  if (progress.startedVerification > 0) {
    return `${progress.startedVerification}/${progress.awaitingVerification} 项已由另一位专家开始核对，其余等待排队；系统也会运行必需检查。`;
  }
  return '实现专家已提交结果，系统将运行必需检查，并安排另一位专家核对；通过后子任务才算完成。';
}

export function eventLabel(event: MissionEvent) {
  const labels: Record<string, string> = {
    mission_created: '创建任务',
    mission_handoff_received: '接收协作任务',
    mission_roles_assigned: '应用角色指派',
    plan_proposed: '生成计划',
    plan_confirmed: '确认计划',
    task_dispatched: '派发任务',
    task_baseline_captured: '记录执行前文件基线',
    verification_retry_requested: '重新核验同一实现',
    task_dispatch_deferred: '等待执行恢复',
    mission_supervisor_error: '系统暂时无法推进',
    verification_preparing: '系统检查任务结果',
    verification_started: '另一位专家开始核验',
    host_verification_checked: '宿主证据验收',
    task_repaired: '修复验收通过',
    task_reopened_for_review: '复核退回原专家修改',
    mission_pause: '暂停任务',
    mission_resume: '恢复任务',
    runtime_workspace_blocked: '命令环境待处理',
    runtime_workspace_resolved: '命令环境已处理',
    runtime_workspace_recovery_failed: '命令环境仍需处理',
    mission_permission_changed: '执行权限已调整',
    budget_exhausted: '预算已用尽',
    budget_extended: '增加预算并继续',
    mission_cancel: '取消任务',
    task_started: '开始执行',
    task_failed: '任务失败',
    task_progress: '进度',
    task_awaiting_verification: '等待验收',
    verification_failed: '验收失败',
    verification_runtime_failed: '核验执行器未完成',
    follow_up_tasks_added: '新增跟进任务',
    mission_follow_up_failed: '返工计划生成失败',
    mission_follow_up_waiting: '返工计划待恢复',
    run_usage_reconciled: '补记运行用量',
    task_completed: '任务完成',
    mission_completed: '任务完成',
    mission_waiting: '暂停或等待',
    mission_failed: '任务失败',
  };
  return labels[event.event_type] || event.event_type;
}

export function taskTitle(mission: MissionView, taskId: string) {
  if (!taskId) return '';
  return mission.tasks.find(task => task.spec.id === taskId)?.spec.title || taskId;
}

/** 没有细粒度上游事件时，界面只能诚实展示状态和最近事件。 */
export function progressDisclosure(mission: MissionView, events: MissionEvent[]) {
  if (events.length) return '按实际运行事件更新';
  if (mission.status === 'active') return '当前执行器暂未上报细粒度事件；显示运行状态，不推断具体操作';
  return '尚无执行事件';
}


/** One sentence the list can show without opening the task. */
export function missionNextAction(mission: MissionView, events: MissionEvent[] = []) {
  switch (mission.status) {
    case 'draft': case 'planned': return '生成执行计划';
    case 'awaiting_assignment': return '选择协作专家';
    case 'awaiting_confirmation': return '审查计划与访问范围';
    case 'active':
      {
        if (missionStalled(mission)) {
          const repairs = mission.tasks.filter(task => task.spec.repair_of).length;
          return mission.budget && repairs >= mission.budget.max_repair_attempts
            ? '核验或执行未通过，返工次数已用完；查看失败原因'
            : '任务已停下，查看失败原因与跟进方案';
        }
        const latest = events[events.length - 1];
        if (latest?.event_type === 'mission_supervisor_error') return '系统遇到问题，将自动重试';
        if (latest?.event_type === 'verification_preparing') return '系统正在运行必需检查';
        const progress = missionProgress(mission);
        if (progress.running && progress.awaitingVerification) return `${progress.running} 项执行中，${progress.awaitingVerification} 项结果待核验`;
        if (progress.running) return '专家正在执行子任务';
        if (progress.awaitingVerification) {
          const notStarted = progress.awaitingVerification - progress.startedVerification;
          if (progress.startedVerification && notStarted) return `${progress.startedVerification} 项核验中，${notStarted} 项待另一位专家核验`;
          return progress.startedVerification ? `${progress.startedVerification} 项由另一位专家核验中` : `${progress.awaitingVerification} 项结果待核验`;
        }
      }
      if (mission.tasks.some(task => task.status === 'failed')) return '查看失败原因与跟进任务';
      if (mission.tasks.some(task => task.status === 'ready')) return '有子任务已就绪，等待派发';
      if (mission.tasks.some(task => task.status === 'blocked')) return '部分子任务等待前置任务';
      return '等待宿主继续执行';
    case 'waiting': {
      if (missionRuntimeBlocked(mission, events)) return '选择工作区授权或仅用文件工具继续';
      const lastEvent = events[events.length - 1];
      if (lastEvent?.event_type === 'mission_follow_up_waiting') return '返工计划生成失败；处理原因后恢复任务重试';
      if (lastEvent?.event_type === 'verification_runtime_failed') return '核验执行器未完成；恢复后可重新核验同一交付';
      return lastEvent?.event_type === 'budget_exhausted' ? '增加预算并继续' : lastEvent?.event_type === 'mission_pause' ? '任务已暂停，可恢复执行' : '查看等待原因';
    }
    case 'completed': return '查看交付结果与验收证据';
    case 'failed': return '查看失败原因';
    case 'canceled': return '任务已取消';
  }
}
