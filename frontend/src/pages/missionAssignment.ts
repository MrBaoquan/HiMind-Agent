import type { ExpertSummary, MissionAssignmentOptions, MissionExecutionOption, MissionRoleAssignment } from '../services/agentApi';

export function expertKey(expert: { id: string; version: string; digest: string }) {
  return `${expert.id}@${expert.version}:${expert.digest}`;
}

export function bindExpert(role: MissionRoleAssignment, expert: ExpertSummary | undefined): MissionRoleAssignment {
  return { ...role, expert: expert ? { expert_id: expert.id, expert_version: expert.version, expert_digest: expert.digest } : null };
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
