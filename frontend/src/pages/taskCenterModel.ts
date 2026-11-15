import type { AgentActivityItem, MissionView, WorkflowCenterSnapshot } from '../services/agentApi';
import { missionActionActor, missionResumeAction, missionRunningVerifications, missionNextAction, missionStalled, missionWait } from './missionView.ts';

export const ENDED_STATUSES = new Set(['completed', 'succeeded', 'success', 'done', 'finished', 'failed', 'canceled', 'cancelled']);
export type MissionTodo = { id: string; missionId: string; title: string; action: string; kind: 'assignment' | 'plan' | 'confirmation' | 'attention' | 'budget' | 'environment' | 'verification_resume'; cursor?: number; reason?: string; actor?: string };
export function missionTodo(mission: MissionView, activities: AgentActivityItem[] = []): MissionTodo | null {
  if (ENDED_STATUSES.has(mission.status)) return null;
  if (missionRunningVerifications(mission, activities).length) return null;
  const wait = missionWait(mission);
  const resume = missionResumeAction(mission, activities);
  const kind = resume?.kind === 'verification' && !resume.optional ? 'verification_resume'
    : mission.status === 'waiting' && wait?.actor === 'user' && wait.reason === 'budget_exhausted' ? 'budget'
    : mission.status === 'waiting' && wait?.actor === 'user' && wait.environment_blocked ? 'environment'
    : missionStalled(mission) ? 'attention'
    : mission.status === 'awaiting_assignment' ? 'assignment'
    : mission.status === 'draft' || mission.status === 'planned' ? 'plan'
    : mission.status === 'awaiting_confirmation' ? 'confirmation' : null;
  if (!kind) return null;
  return { id: ['mission', mission.mission_id, kind, mission.plan_sha256 || mission.plan_revision].join(':'),
    missionId: mission.mission_id, title: mission.title, action: missionNextAction(mission, [], activities), kind,
    cursor: wait?.cursor, reason: wait?.reason, actor: missionActionActor(mission, activities) };
}
export function activityNeedsUser(item: Pick<AgentActivityItem, 'status' | 'pause_kind'>) {
  return item.status === 'waiting' && ['approval', 'feedback', 'form', 'evidence'].includes(item.pause_kind || '');
}
export function executionTodoId(runId: string, stepId: string, kind: string) {
  return ['run', runId, stepId, kind].join(':');
}
export function workflowNeedsUser(item: WorkflowCenterSnapshot['runs'][number]) {
  if (item.run.status !== 'waiting') return false;
  return ['approval', 'feedback', 'form', 'evidence'].includes(item.interaction_request?.kind || item.waiting_kind);
}
export function activityStatusLabel(item: Pick<AgentActivityItem, 'status' | 'pause_kind'>) {
  if (item.status !== 'waiting') return '';
  return ({ approval: '待审批', feedback: '待反馈', form: '待填写信息', evidence: '待提交证据',
    user: '已暂停', external: '等待外部状态' } as Record<string, string>)[item.pause_kind || ''] || '等待中';
}
export function clientLabel(value: string) {
  return ({codex:'Codex', 'claude-code':'Claude Code', 'agent-ui':'HiMind Agent', 'himind-agent':'HiMind Agent',
    'builtin-ai':'AI 对话', copilot:'Copilot'} as Record<string,string>)[value] || value || '未记录';
}
export function workspaceKey(value: string) {
  const normalized = value.trim().replace(/\\/g, '/').replace(/\/+$/, '');
  return /^[a-z]:/i.test(normalized) ? normalized.toLowerCase() : normalized;
}
export type TaskProjection = { id: string; kind: 'mission' | 'activity'; mission?: MissionView; activity?: AgentActivityItem;
  title: string; workspace: string; client: string; method: string; trigger: string; status: string;
  updatedAt: string; needsUser: boolean; ended: boolean; search: string };
export function projectTasks(missions: MissionView[], activities: AgentActivityItem[], statusActivities: AgentActivityItem[] = activities) {
  const missionIds = new Set(missions.map(m => m.mission_id));
  const byRun = new Map(activities.filter(a => a.workflow_run_id).map(a => [a.workflow_run_id!, a]));
  const byAttempt = new Map(missions.flatMap(m => m.tasks.filter(t => t.attempt_run_id).map(t => [t.attempt_run_id, m.mission_id] as const)));
  const owner = (item: AgentActivityItem): string => {
    const visited = new Set<string>();
    let current: AgentActivityItem | undefined = item;
    while (current) {
      if (visited.has(current.id)) return '';
      visited.add(current.id);
      if (current.mission_id && missionIds.has(current.mission_id)) return 'mission:' + current.mission_id;
      const mission = byAttempt.get(current.workflow_run_id || '');
      if (mission) return 'mission:' + mission;
      const parent: AgentActivityItem | undefined = byRun.get(current.parent_run_id || '');
      if (!parent) return current === item ? '' : 'activity:' + current.id;
      current = parent;
    }
    return '';
  };
  const related = new Map<string, AgentActivityItem[]>();
  const top = activities.filter(item => {
    const parent = owner(item);
    if (!parent) return true;
    related.set(parent, [...(related.get(parent) || []), item]);
    return false;
  });
  const rows: TaskProjection[] = [
    ...missions.map(m => ({ id: 'mission:' + m.mission_id, kind: 'mission' as const, mission: m,
      title: m.title, workspace: m.workspace.root, client: clientLabel(m.handoff?.source_client || ''),
      method: '协作任务', trigger: m.handoff?.source_client === 'agent-ui' ? 'Agent 中创建' : m.handoff ? '会话交接' : '', status: m.status, updatedAt: m.updated_at,
      needsUser: !ENDED_STATUSES.has(m.status) && (missionStalled(m) || Boolean(missionTodo(m, statusActivities)) || (related.get('mission:'+m.mission_id) || []).some(activityNeedsUser)), ended: ENDED_STATUSES.has(m.status),
      search: [m.mission_id, m.title, m.objective, m.workspace.root, m.handoff?.source_client, m.handoff?.context_summary].join(' ') })),
    ...top.map(a => ({ id: 'activity:' + a.id, kind: 'activity' as const, activity: a, title: a.title,
      workspace: a.workspace_root || '', client: clientLabel(a.source_client || (a.source === 'dashboard' ? '工作台' : '')),
      method: a.source === 'dashboard' ? '工作台任务' : a.workflow_run_id ? '工作流运行' : '技能运行',
      trigger: a.source === 'schedule' || a.trigger_kind === 'schedule' ? '定时触发' : '',
      status: a.status, updatedAt: a.finished_at || a.updated_at || a.created_at,
      needsUser: !ENDED_STATUSES.has(a.status) && (activityNeedsUser(a) || (related.get('activity:'+a.id) || []).some(activityNeedsUser)), ended: ENDED_STATUSES.has(a.status),
      search: [a.id, a.title, a.subtitle, a.detail, a.error, a.workspace_root, a.source_client].join(' ') })),
  ];
  rows.sort((a,b) => (Date.parse(b.updatedAt) || 0) - (Date.parse(a.updatedAt) || 0));
  return { rows, related };
}
export type TaskListContext = { query: string; workspace: string; view: 'all' | 'attention' | 'running' | 'ended'; method: string; scrollTop: number };
export const EMPTY_TASK_CONTEXT: TaskListContext = { query:'', workspace:'', view:'all', method:'', scrollTop:0 };
