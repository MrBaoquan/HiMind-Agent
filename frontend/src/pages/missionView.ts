import type { MissionEvent, MissionStatus, MissionTaskStatus, MissionView } from '../services/agentApi';

export const MISSION_STATUS_LABELS: Record<MissionStatus, string> = {
  draft: '待规划',
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
  if (mission.status === 'draft' || mission.status === 'planned') return 'review';
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
    plan_proposed: '生成计划',
    plan_confirmed: '确认计划',
    task_dispatched: '派发任务',
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
