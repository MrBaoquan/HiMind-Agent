import type { ExpertSummary, MissionAssignmentOptions, MissionExecutionOption, MissionRoleAssignment, MissionRoleOption, MissionView } from '../services/agentApi';

export function expertKey(expert: { id: string; version: string; digest: string }) {
  return `${expert.id}@${expert.version}:${expert.digest}`;
}

export function bindExpert(role: MissionRoleAssignment, expert: ExpertSummary | undefined): MissionRoleAssignment {
  return { ...role, expert: expert ? { expert_id: expert.id, expert_version: expert.version, expert_digest: expert.digest } : null };
}

export function recommendedRole(option: MissionRoleOption, options: MissionAssignmentOptions): MissionRoleAssignment {
  const pin = option.recommended_expert;
  const expert = pin && options.experts.find(value => value.id === pin.expert_id && value.version === pin.expert_version && value.digest === pin.expert_digest);
  const executor = options.executors.find(value => value.executor === option.recommended_executor);
  const model = executor?.models.find(value => value.alias === option.recommended_model && value.service_id === option.recommended_service_id);
  return bindExpert({
    role_id: option.role_id, label: option.label, executor: option.recommended_executor,
    service_id: model?.service_id || '', model: model?.alias || '',
  }, expert || undefined);
}

export function initialAssignmentRoles(mission: MissionView, options: MissionAssignmentOptions): MissionRoleAssignment[] {
  const referenced = new Set(mission.proposed_tasks.map(task =>
    mission.assignment?.task_roles.find(mapping => mapping.task_id === task.id)?.role_id || task.execution.role_id));
  return options.roles.flatMap(option => {
    const existing = mission.assignment?.roles.find(role => role.role_id === option.role_id);
    if (existing) return [{ ...existing, label: option.label }];
    const role = recommendedRole(option, options);
    // An omitted optional role in a saved assignment is a deliberate choice.
    if (option.optional && !referenced.has(option.role_id) && (mission.assignment || !role.expert)) return [];
    return [role];
  });
}

export function initialTaskRoles(mission: MissionView, roles: MissionRoleAssignment[]): Record<string, string> {
  const fallback = roles.find(role => role.expert)?.role_id || roles[0]?.role_id || '';
  return Object.fromEntries(mission.proposed_tasks.map(task => [task.id,
    mission.assignment?.task_roles.find(mapping => mapping.task_id === task.id)?.role_id || task.execution.role_id || fallback]));
}

export function hasMissingTaskAssignment(mission: MissionView, taskRoles: Record<string, string>, roles: MissionRoleAssignment[]): boolean {
  return mission.proposed_tasks.some(task => !roles.some(role => role.role_id === taskRoles[task.id]));
}

export function serviceOptions(executor: MissionExecutionOption | undefined) {
  return [...new Map((executor?.models || []).map(model => [model.service_id, { id: model.service_id, label: model.service_label }])).values()];
}

export function changeExecutor(role: MissionRoleAssignment, executor: MissionExecutionOption): MissionRoleAssignment {
  const first = executor.models[0];
  return { ...role, executor: executor.executor, service_id: first?.service_id || '', model: first?.alias || '' };
}

export function changeService(role: MissionRoleAssignment, executor: MissionExecutionOption | undefined, serviceId: string): MissionRoleAssignment {
  return { ...role, service_id: serviceId, model: executor?.models.find(model => model.service_id === serviceId)?.alias || '' };
}

export function assignmentProblem(role: MissionRoleAssignment, options: MissionAssignmentOptions): string {
  const pin = role.expert;
  if (!pin || !options.experts.some(expert => expert.id === pin.expert_id && expert.version === pin.expert_version && expert.digest === pin.expert_digest)) return '请选择已安装专家';
  const executor = options.executors.find(option => option.executor === role.executor);
  if (!executor?.available) return executor?.reason || '所选执行器不可用';
  if ((role.executor === 'himind.builtin' || executor.models.length > 0) && !executor.models.some(model => model.alias === role.model && model.service_id === role.service_id)) return '请选择可用服务与模型';
  if (executor.models.length === 0 && (role.model || role.service_id)) return '该执行器不接受服务或模型覆盖';
  return '';
}
