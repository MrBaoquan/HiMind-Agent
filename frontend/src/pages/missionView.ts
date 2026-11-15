import type { AgentActivityItem, MissionEvent, MissionStatus, MissionStatus as MissionStatusType, MissionTaskStatus, MissionView } from '../services/agentApi';

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

/** Active without a worker is not proof of failure or a request for human action. */
export function missionStalled(mission: MissionView) {
  return mission.status === 'failed';
}

/** Accept only the backend projection associated with this confirmed plan. */
export function missionWait(mission: MissionView) {
  const wait = mission.effective_wait;
  return wait?.contract_version === 'mission.wait.v1' && Boolean(mission.plan_sha256)
    && wait.plan_sha256 === mission.plan_sha256
    && mission.confirmation?.plan_sha256 === mission.plan_sha256
    && Number.isSafeInteger(wait.cursor) && wait.cursor >= 0 ? wait : null;
}

/** can_resume is a host admission, including ownership; the UI never invents it.
 * mission.wait.v1 historically called admitted, paused verification "system".
 * Normalize the user action, retaining the original DTO for diagnostics/history.
 */
/** A dispatched ID or historical start is not a live process. Require explicit
 * persisted mission/task ownership and the current, distinct verification run. */
export function missionRunningVerifications(mission: MissionView, activities: AgentActivityItem[] = []) {
  if (!activities.length || !['active', 'waiting'].includes(mission.status) || !missionWait(mission)) return [];
  return mission.tasks.filter(task => task.status === 'awaiting_verification'
    && task.attempt_run_id && task.verification_run_id && task.attempt_run_id !== task.verification_run_id
    && activities.some(item => item.status === 'running' && item.mission_id === mission.mission_id
      && item.mission_task_id === task.spec.id && item.workflow_run_id === task.verification_run_id));
}

export function missionResumeAction(mission: MissionView, activities: AgentActivityItem[] = []) {
  const wait = missionWait(mission);
  if (mission.status !== 'waiting' || wait?.can_resume !== true || wait.environment_blocked
      || mission.tasks.some(task => task.status === 'running') || missionRunningVerifications(mission, activities).length) return null;
  const pending = mission.tasks.some(task => task.status === 'awaiting_verification');
  const verificationActor = wait.reason === 'system_verification'
    ? ['system', 'user'].includes(wait.actor) : wait.actor === 'user';
  if (pending && mission.tasks.some(task => task.status === 'awaiting_verification' && Boolean(task.attempt_run_id))
      && verificationActor && ['system_verification', 'verification_resume_required', 'manual_pause'].includes(wait.reason)) {
    const optional = wait.reason === 'manual_pause';
    return {kind: 'verification' as const, label: '续跑待核验', optional,
      responsibility: optional ? '由你决定何时继续 · 当前无需处理' : '需要你手动续跑 · 系统随后检查与独立核验',
      explanation: '任务已暂停；请手动续跑检查与独立核验，不重做实现。'};
  }
  if (!pending && wait.reason === 'manual_pause' && wait.actor === 'user') {
    return {kind: 'tasks' as const, label: '续跑未完成任务', optional: true,
      responsibility: '由你决定何时继续 · 当前无需处理',
      explanation: '由你决定何时续跑；继续尚未完成的任务'};
  }
  return null;
}

export function missionActionActor(mission: MissionView, activities: AgentActivityItem[] = []) {
  return missionRunningVerifications(mission, activities).length ? 'system' : missionResumeAction(mission, activities) ? 'user' : missionWait(mission)?.actor;
}

export function missionResponsibility(mission: MissionView, activities: AgentActivityItem[] = []) {
  if (missionRunningVerifications(mission, activities).length) return '另一位专家核验中 · 当前无需你操作';
  const resume = missionResumeAction(mission, activities);
  if (resume) return resume.responsibility;
  const wait = missionWait(mission);
  if (mission.status === 'waiting') return wait?.actor === 'user'
    ? wait.reason === 'manual_pause' ? '由你决定何时继续 · 当前无需处理' : '需要你处理'
    : wait?.actor === 'system' ? '系统核对恢复资格 · 当前无可用续跑操作' : '处理者未确认 · 请刷新状态';
  if (['draft','planned','awaiting_assignment','awaiting_confirmation'].includes(mission.status)) return '需要你处理';
  if (mission.status !== 'active') return '当前无需你操作';
  const running = mission.tasks.filter(task => task.status === 'running');
  if (running.length) {
    const actors = [...new Set(running.map(task => {
      const execution = task.spec?.execution;
      const role = mission.assignment?.roles.find(role => role.role_id === execution?.role_id);
      return role?.label || (execution?.executor === 'personal.codex' ? 'Codex 实现专家' : '实现专家');
    }))];
    return `${actors.join('、')}执行中 · 无需你操作`;
  }
  return mission.tasks.some(task => task.status === 'awaiting_verification')
    ? '系统检查与另一位专家核验 · 无需你操作' : '执行者未确认 · 等待系统更新';
}

/** Repairs remain attempts of an original delivery, including failed history. */
export function missionDeliveries(mission: MissionView) {
  const byId = new Map(mission.tasks.map((task, index) => [task.spec?.id || `legacy-${index}`, task]));
  const groups = new Map<string, { root: typeof mission.tasks[number]; attempts: typeof mission.tasks; done: boolean }>();
  const unassociated: typeof mission.tasks = [];
  for (const [id, task] of byId) {
    let current = task; let rootId = id; const seen = new Set<string>(); let valid = true;
    while (current.spec?.repair_of) {
      if (seen.has(rootId)) { valid = false; break; }
      seen.add(rootId);
      rootId = current.spec.repair_of;
      const parent = byId.get(rootId);
      if (!parent) { valid = false; break; }
      current = parent;
    }
    if (!valid) { unassociated.push(task); continue; }
    const group = groups.get(rootId) || {root: current, attempts: [], done: false};
    group.attempts.push(task); groups.set(rootId, group);
  }
  for (const group of groups.values()) {
    const selected = group.attempts.filter(task => task.status !== 'canceled');
    const leaves = selected.filter(task => !selected.some(child => child.spec?.repair_of && child.spec.repair_of === task.spec?.id));
    group.done = leaves.length > 0 && leaves.every(task => task.status === 'completed');
  }
  return { groups: [...groups.values()].filter(group => group.attempts.some(task => task.status !== 'canceled')), unassociated };
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
  if (!pending.some(task => task.status === 'running')) return 'checking';
  return 'execute';
}
export const MISSION_STAGE_LABELS = {
  assignment: '配置专家', plan: '制定计划', confirm: '审查计划', execute: '执行子任务',
  verify: '独立验收', checking: '等待系统更新', attention: '需要处理', waiting: '暂停或等待', completed: '已完成', failed: '执行失败', canceled: '已取消',
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
  const deliveries = missionDeliveries(mission);
  return {
    done: deliveries.groups.filter(group => group.done).length,
    total: deliveries.groups.length,
    running: selected.filter(task => task.status === 'running').length,
    awaitingVerification: selected.filter(task => task.status === 'awaiting_verification').length,
    startedVerification: selected.filter(task => task.status === 'awaiting_verification' && Boolean(task.verification_run_id)).length,
    ready: selected.filter(task => task.status === 'ready').length,
    blocked: selected.filter(task => task.status === 'blocked').length,
    failed: selected.filter(task => task.status === 'failed').length,
  };
}

/** Events describe history; current gates only come from the associated DTO. */
export function missionRuntimeBlocked(mission: MissionView, _events: MissionEvent[] = []) {
  return mission.status === 'waiting' && missionWait(mission)?.environment_blocked === true;
}

export function missionStatusLabel(mission: MissionView, _events: MissionEvent[] = [], activities: AgentActivityItem[] = []) {
  if (missionRunningVerifications(mission, activities).length) return mission.status === 'waiting' ? '核验进行中 · 后续派发已暂停' : '核验进行中';
  const resume = missionResumeAction(mission, activities);
  if (resume?.kind === 'verification' && !resume.optional) return '待手动续跑核验';
  if (mission.status === 'waiting') {
    const wait = missionWait(mission);
    return ({budget_exhausted:'预算已用尽', environment_blocked:'命令环境待处理', manual_pause:'已暂停',
      system_verification:'等待系统检查或核验'} as Record<string,string>)[wait?.reason || ''] || '等待原因未确认';
  }
  if (mission.status === 'active') {
    const progress = missionProgress(mission);
    if (progress.running) return '专家执行中';
    if (progress.awaitingVerification) return progress.startedVerification ? '核验任务已派发' : '待核验';
    return '状态待核对';
  }
  return MISSION_STATUS_LABELS[mission.status];
}

export function missionVerificationExplanation(mission: MissionView, _events: MissionEvent[] = [], activities: AgentActivityItem[] = []) {
  if (!mission.tasks.some(task => task.status === 'awaiting_verification')) return '';
  if (missionRunningVerifications(mission, activities).length) return mission.status === 'waiting'
    ? '独立核验仍在运行，可完成当前步骤。后续派发已暂停，不启动新核验或重做实现。'
    : '关联的独立核验正在运行；请查看核验详情，不重复启动核验或执行已完成的实现。';
  const resume = missionResumeAction(mission, activities);
  if (resume?.kind === 'verification') return resume.explanation;
  if (mission.status === 'waiting') return '实现已提交，任务仍在暂停或等待。未确认恢复前不重做实现；检查与独立验收分别记录。';
  return '系统将运行必需检查并由另一位专家核对。均通过后才算交付；未报告运行状态时不推断进度。';
}

export function eventLabel(event: MissionEvent) {
  const increase = event.payload?.increase;
  const increaseRecord = increase && typeof increase === 'object' ? increase as Record<string, unknown> : null;
  if (event.event_type === 'budget_extended' && parseLegacyVerificationOverride(increaseRecord?.legacy_verification_override)) {
    return '补充核验时限，任务保持等待';
  }
  const labels: Record<string, string> = {
    mission_created: '创建任务',
    mission_feedback_submitted: '收到审查意见，供后续执行参考',
    mission_follow_up_format_retry: '系统重试整理跟进方案',
    mission_handoff_received: '接收协作任务',
    mission_roles_assigned: '应用角色指派',
    plan_proposed: '生成计划',
    plan_confirmed: '确认计划',
    task_dispatched: '派发任务',
    task_baseline_captured: '记录执行前文件基线',
    verification_retry_override_authorized: '确认本次旧记录人工恢复授权',
    verification_retry_requested: '重新核验同一实现',
    task_dispatch_deferred: '等待执行恢复',
    mission_supervisor_error: '系统暂时无法推进',
    verification_preparing: '系统检查任务结果',
    verification_checks_completed: '宿主必需检查已记录',
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
export function missionNextAction(mission: MissionView, _events: MissionEvent[] = [], activities: AgentActivityItem[] = []) {
  if (missionRunningVerifications(mission, activities).length) return mission.status === 'waiting'
    ? '查看在途核验；当前步骤可收尾，后续派发已暂停' : '查看在途核验；不重复启动';
  switch (mission.status) {
    case 'draft': case 'planned': return '生成执行计划';
    case 'awaiting_assignment': return '选择协作专家';
    case 'awaiting_confirmation': return '审查计划与访问范围';
    case 'active': {
      const progress = missionProgress(mission);
      if (progress.running && progress.awaitingVerification) return `${progress.running} 项执行中，${progress.awaitingVerification} 项结果待核验`;
      if (progress.running) return '专家正在执行子任务';
      if (progress.awaitingVerification) return progress.startedVerification
        ? `${progress.startedVerification} 项核验任务已派发，等待状态更新` : `${progress.awaitingVerification} 项结果待核验`;
      return '等待系统更新执行或跟进状态；可查看最近记录';
    }
    case 'waiting': {
      const resume = missionResumeAction(mission);
      if (resume) return resume.kind === 'verification' ? '续跑待核验；不重复执行已完成的实现' : resume.explanation;
      const wait = missionWait(mission);
      if (wait?.reason === 'budget_exhausted') return wait.can_increase_budget ? '增加预算并继续' : '预算已用尽，先核对运行状态';
      if (wait?.environment_blocked) return '选择工作区授权或仅用文件工具继续';
      if (wait?.reason === 'manual_pause') return '由你决定何时续跑；继续尚未完成的任务';
      if (wait?.reason === 'system_verification') return '核对核验恢复资格；当前不可续跑，不重复执行实现';
      return '刷新等待原因；资格未确认前不提交恢复或增额';
    }
    case 'completed': return '查看交付结果与验收证据';
    case 'failed': return '查看失败原因';
    case 'canceled': return '任务已取消';
  }
}

function parseLegacyVerificationOverride(value: unknown) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const override = value as Record<string, unknown>;
  if (typeof override.expected_pause_event_id !== 'string' || !override.expected_pause_event_id
      || typeof override.expected_runtime_failure_event_id !== 'string' || !override.expected_runtime_failure_event_id) return null;
  return {
    expected_pause_event_id: override.expected_pause_event_id,
    expected_runtime_failure_event_id: override.expected_runtime_failure_event_id,
  };
}

/** Return the exact old-event pair only while this Mission is at that recovery decision. */
export function legacyVerificationOverrideForTask(
  mission: MissionView,
  events: MissionEvent[],
  task: MissionView['tasks'][number],
) {
  const wait = missionWait(mission);
  if (!wait || mission.status !== 'waiting' || task.status !== 'failed' || task.verification_passed !== false
      || !task.attempt_run_id || !task.verification_run_id || missionRuntimeBlocked(mission, events)
      || mission.confirmation?.plan_sha256 !== mission.plan_sha256
      || mission.tasks.some(item => ['running','ready','awaiting_verification'].includes(item.status))
      || mission.tasks.some(child => child.spec.repair_of === task.spec.id && child.status !== 'canceled')) return null;

  const ordered = [...events].sort((a,b) => a.sequence - b.sequence);
  const latest = ordered[ordered.length - 1];
  if (!latest || latest.sequence !== wait.cursor) return null;

  let expected: ReturnType<typeof parseLegacyVerificationOverride> = null;
  if (latest.event_type === 'verification_runtime_failed') {
    const pause = ordered.find(event => event.sequence + 1 === latest.sequence);
    if (!pause || pause.event_type !== 'mission_pause') return null;
    expected = {
      expected_pause_event_id: pause.event_id,
      expected_runtime_failure_event_id: latest.event_id,
    };
  } else if (latest.event_type === 'budget_extended') {
    const increase = latest.payload?.increase;
    const record = increase && typeof increase === 'object' ? increase as Record<string, unknown> : null;
    expected = parseLegacyVerificationOverride(record?.legacy_verification_override);
    if (!expected || record?.additional_rounds !== 0 || !Number.isSafeInteger(record.additional_minutes)
        || Number(record.additional_minutes) <= 0 || record.additional_repair_attempts !== 0
        || record.additional_tokens != null || record.unlimited_tokens !== false || record.additional_cost != null) return null;
  } else return null;

  if (!expected) return null;
  const pause = ordered.find(event => event.event_id === expected?.expected_pause_event_id);
  const runtime = ordered.find(event => event.event_id === expected?.expected_runtime_failure_event_id);
  const failures = ordered.filter(event => event.task_id === task.spec.id && event.event_type === 'verification_failed');
  const failure = failures[failures.length - 1];
  if (!pause || !runtime || runtime.event_type !== 'verification_runtime_failed' || runtime.task_id !== task.spec.id
      || runtime.sequence !== pause.sequence + 1 || !failure || failure.sequence + 1 !== pause.sequence
      || pause.payload?.action !== 'pause' || Object.prototype.hasOwnProperty.call(pause.payload || {}, 'verification_run_id')
      || Object.prototype.hasOwnProperty.call(runtime.payload || {}, 'automatic_pause')
      || Object.prototype.hasOwnProperty.call(runtime.payload || {}, 'implementation_run_id')
      || Object.prototype.hasOwnProperty.call(runtime.payload || {}, 'plan_sha256')
      || runtime.payload?.verification_run_id !== task.verification_run_id
      || runtime.payload?.recovery_action !== 'mission.verification.retry'
      || failure.payload?.implementation_run_id !== task.attempt_run_id
      || failure.payload?.verification_run_id !== task.verification_run_id) return null;
  for (const event of ordered.filter(event => event.sequence > runtime.sequence)) {
    const increase = event.payload?.increase as Record<string, unknown> | undefined;
    const ids = parseLegacyVerificationOverride(increase?.legacy_verification_override);
    if (event.event_type !== 'budget_extended' || event.payload?.plan_sha256 !== mission.plan_sha256
        || ids?.expected_pause_event_id !== expected.expected_pause_event_id
        || ids?.expected_runtime_failure_event_id !== expected.expected_runtime_failure_event_id
        || increase?.additional_rounds !== 0 || !Number.isSafeInteger(increase.additional_minutes)
        || Number(increase.additional_minutes) <= 0 || increase.additional_repair_attempts !== 0
        || increase.additional_tokens != null || increase.unlimited_tokens !== false || increase.additional_cost != null) return null;
  }
  return expected;
}

/** UI affordance only; the Store re-reads the full ledger under its write lock. */
export function canRetryWaitingVerification(mission: MissionView, events: MissionEvent[], task: MissionView['tasks'][number]) {
  if (mission.status !== 'waiting' || task.status !== 'failed' || task.verification_passed !== false
    || !task.attempt_run_id || !task.verification_run_id || missionRuntimeBlocked(mission, events)
    || mission.confirmation?.plan_sha256 !== mission.plan_sha256
    || mission.tasks.some(item => ['running','ready','awaiting_verification'].includes(item.status))
    || mission.tasks.some(child => child.spec.repair_of === task.spec.id && child.status !== 'canceled')) return false;
  const legacyOverride = legacyVerificationOverrideForTask(mission, events, task);
  const ordered = [...events].sort((a,b) => b.sequence-a.sequence);
  const manualRetryReady = Boolean(legacyOverride
    && (ordered[0]?.event_type === 'budget_extended' || missionWait(mission)?.budget_reason == null));
  const runtime = ordered.find(event => ['mission_pause','mission_resume','budget_exhausted','verification_runtime_failed'].includes(event.event_type));
  if (!runtime || runtime.event_type !== 'verification_runtime_failed' || runtime.task_id !== task.spec.id
    || runtime.payload?.verification_run_id !== task.verification_run_id
    || runtime.payload?.recovery_action !== 'mission.verification.retry') return manualRetryReady;
  const pause = ordered.find(event => event.sequence < runtime.sequence && ['mission_pause','mission_resume','budget_exhausted'].includes(event.event_type));
  const failure = ordered.find(event => event.task_id === task.spec.id && event.event_type === 'verification_failed');
  if (!pause || pause.event_type !== 'mission_pause' || !failure || failure.sequence >= pause.sequence
    || failure.payload?.implementation_run_id !== task.attempt_run_id
    || failure.payload?.verification_run_id !== task.verification_run_id) return false;
  return (runtime.payload?.automatic_pause === true
    && runtime.payload.implementation_run_id === task.attempt_run_id && runtime.payload.plan_sha256 === mission.plan_sha256
    && pause.payload?.verification_run_id === task.verification_run_id) || manualRetryReady;
}

export function legacyBudgetExtensionAction(isLegacyRecovery: boolean, authorized: boolean) {
  if (!isLegacyRecovery) return { disabled: false, label: '确认并继续', description: '' };
  if (!authorized) return {
    disabled: true,
    label: '请先确认人工授权',
    description: '未授权不会提交增额；本次只补核验时限，重试需另行确认。',
  };
  return {
    disabled: false,
    label: '补充时限并保持等待',
    description: '本次只增加核验时限，任务仍保持等待；之后需单独确认重试同一交付核验。',
  };
}
