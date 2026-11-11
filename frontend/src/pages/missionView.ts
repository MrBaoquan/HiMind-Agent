import type { MissionEvent, MissionStatus, MissionStatus as MissionStatusType, MissionTaskStatus, MissionView } from '../services/agentApi';

export const MISSION_STATUS_LABELS: Record<MissionStatus, string> = {
  draft: '待规划',
  awaiting_assignment: '待指派',
  planned: '计划已生成',
  awaiting_confirmation: '待确认',
  active: '执行中',
  waiting: '等待处理',
  completed: '已完成',
  failed: '已失败',
  canceled: '已取消',
};

export const MISSION_TASK_STATUS_LABELS: Record<MissionTaskStatus, string> = {
  proposed: '待编排',
  blocked: '等待依赖',
  ready: '可派发',
  running: '执行中',
  awaiting_verification: '待验收',
  failed: '失败',
  completed: '已完成',
  canceled: '未选中',
};

/** 把后端阶段映射为固定的四段业务叙事，避免每个调用方自造阶段词。 */
export function missionStage(mission: MissionView): 'review' | 'confirm' | 'optimize' | 'accept' {
  if (mission.status === 'draft' || mission.status === 'awaiting_assignment' || mission.status === 'planned') return 'review';
  if (mission.status === 'awaiting_confirmation') return 'confirm';
  const verifying = mission.tasks.some(task => task.status === 'awaiting_verification');
  if (mission.status === 'completed' || verifying) return 'accept';
  return 'optimize';
}

export const MISSION_STAGE_LABELS = {
  review: '审查与规划',
  confirm: '确认计划',
  optimize: '执行与返工',
  accept: '验收',
} as const;

export function missionProgress(mission: MissionView) {
  const selected = mission.tasks.filter(task => task.status !== 'canceled');
  const done = selected.filter(task => task.status === 'completed').length;
  return { done, total: selected.length };
}

export function eventLabel(event: MissionEvent) {
  const labels: Record<string, string> = {
    mission_created: '创建任务',
    mission_handoff_received: '接收协作任务',
    mission_roles_assigned: '应用角色指派',
    plan_proposed: '生成计划',
    plan_confirmed: '确认计划',
    task_dispatched: '派发任务',
    task_dispatch_deferred: '等待执行恢复',
    host_verification_checked: '宿主证据验收',
    task_repaired: '修复验收通过',
    mission_pause: '暂停任务',
    mission_resume: '恢复任务',
    mission_cancel: '取消任务',
    task_started: '开始执行',
    task_failed: '任务失败',
    task_progress: '进度',
    task_awaiting_verification: '等待验收',
    verification_failed: '验收失败',
    follow_up_tasks_added: '新增跟进任务',
    task_completed: '任务完成',
    mission_completed: '任务完成',
    mission_waiting: '等待处理',
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
export function missionNextAction(mission: MissionView) {
  switch (mission.status) {
    case 'draft': return '下一步：生成执行计划';
    case 'awaiting_assignment': return '下一步：确认角色与服务';
    case 'planned': return '下一步：生成并审查计划';
    case 'awaiting_confirmation': return '下一步：确认计划和访问范围';
    case 'active':
      if (mission.tasks.some(task => task.status === 'awaiting_verification')) return '下一步：独立验收';
      if (mission.tasks.some(task => task.status === 'running')) return '正在执行';
      if (mission.tasks.some(task => task.status === 'failed')) return '下一步：处理失败任务';
      return '下一步：派发可执行任务';
    case 'waiting': return '下一步：恢复或取消';
    case 'completed': return '下一步：沉淀为工作流';
    case 'failed': return '已失败，请查看事件';
    case 'canceled': return '已取消';
  }
}
