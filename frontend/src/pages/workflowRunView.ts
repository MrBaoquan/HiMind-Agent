// 运行动态的纯推导逻辑：把 Run 快照变成「时间线 + 动态流」。
// 独立成模块有两个理由：
//   1. 可以脱离 UI 直接验证（真实事件的 payload 极其庞大，容错必须能测）；
//   2. 动态流只允许输出小字段摘要，绝不把 Step 输出原样贴进界面。
import type { WorkflowInteractionRequest, WorkflowRunSnapshot } from '../services/agentApi';

export type RunStepState = 'done' | 'active' | 'waiting' | 'failed' | 'canceled' | 'skipped' | 'pending';

export type RunTimelineStep = {
  id: string;
  title: string;
  kind: string;
  capabilityId: string;
  executionMode: string;
  state: RunStepState;
  /** 已完成取实际耗时，运行中取「至今」，其余为 0。 */
  durationSeconds: number;
  running: boolean;
  error: string;
  artifacts: Array<{ id: string; name: string }>;
  startedAt: number;
  attempt: number;
};

export type RunActivityItem = {
  key: string;
  at: number;
  clock: string;
  stepTitle: string;
  kind: 'started' | 'completed' | 'progress' | 'error' | 'question' | 'approval' | 'interaction' | 'info';
  text: string;
  detail: string;
};

export type RunTimeline = {
  steps: RunTimelineStep[];
  completed: number;
  total: number;
  percent: number;
  current: RunTimelineStep | null;
  active: boolean;
  /** 已运行时长（活动中的运行按当前时间算）。 */
  elapsedSeconds: number;
  /** 距离最后一次事件/更新的秒数，用来回答「它还活着吗」。 */
  idleSeconds: number;
  startedAt: number;
  headline: string;
};

const ACTIVE_STATUSES = new Set(['queued', 'running', 'waiting']);

// 时间字段在契约里是「unix 秒字符串」，但历史数据里也出现过 ISO 字符串。
export function toEpochSeconds(value: unknown): number {
  if (typeof value === 'number' && Number.isFinite(value)) return Math.floor(value);
  const text = typeof value === 'string' ? value.trim() : '';
  if (!text) return 0;
  const numeric = Number(text);
  if (Number.isFinite(numeric) && text.length >= 9) return Math.floor(numeric);
  const parsed = Date.parse(text);
  return Number.isFinite(parsed) ? Math.floor(parsed / 1000) : 0;
}

export function formatClock(value: number): string {
  if (!value) return '--:--:--';
  const date = new Date(value * 1000);
  const pad = (part: number) => String(part).padStart(2, '0');
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

// 中文口径的时长：说人话比显示 "00:57" 更省心。
export function formatElapsedCn(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  if (total < 60) return `${total} 秒`;
  if (total < 3600) {
    const minutes = Math.floor(total / 60);
    const rest = total % 60;
    return rest ? `${minutes} 分 ${rest} 秒` : `${minutes} 分`;
  }
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  return minutes ? `${hours} 小时 ${minutes} 分` : `${hours} 小时`;
}

export function formatIdleCn(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  if (total < 5) return '刚刚';
  if (total < 60) return `${total} 秒前`;
  if (total < 3600) return `${Math.floor(total / 60)} 分钟前`;
  return `${Math.floor(total / 3600)} 小时前`;
}

function stepState(status: string, runStatus: string): RunStepState {
  switch (status) {
    case 'succeeded':
      return 'done';
    case 'running':
      return 'active';
    case 'waiting':
      return 'waiting';
    case 'failed':
      return 'failed';
    case 'canceled':
      return 'canceled';
    case 'skipped':
      return 'skipped';
    default:
      return runStatus === 'succeeded' ? 'done' : 'pending';
  }
}

function asObject(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}

function asText(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

function asNumber(value: unknown): number {
  if (typeof value === 'number' && Number.isFinite(value)) return value;
  const parsed = Number(asText(value));
  return Number.isFinite(parsed) ? parsed : 0;
}

function truncate(text: string, limit: number): string {
  const normalized = text.replace(/\s+/g, ' ').trim();
  return normalized.length > limit ? `${normalized.slice(0, limit)}…` : normalized;
}

function interactionActionLabel(action: string) {
  return ({
    approve_or_reject: '需要批准或拒绝',
    submit_feedback: '需要提交反馈',
    submit_form: '需要填写表单',
    submit_evidence: '需要提交证据',
    inspect_run: '需要检查运行状态',
  } as Record<string, string>)[action] || action;
}

function interactionActivity(snapshot: WorkflowRunSnapshot): RunActivityItem | null {
  const request: WorkflowInteractionRequest | null | undefined = snapshot.interaction_request;
  if (!request || request.status !== 'pending') return null;
  const at = toEpochSeconds(request.created_at || '');
  const kind = request.kind === 'feedback' ? 'question' : request.kind === 'approval' ? 'approval' : 'interaction';
  return {
    key: `interaction:${request.id}`,
    at,
    clock: formatClock(at),
    stepTitle: request.title || request.step_id || '运行',
    kind,
    text: request.title || '运行需要你的处理',
    detail: [request.description, interactionActionLabel(request.required_action)].filter(Boolean).join(' · '),
  };
}

/** 每个步骤产出了哪些产物：从 tool_completed 的 artifact_ids 反查名字。 */
function artifactsByStep(snapshot: WorkflowRunSnapshot): Map<string, Array<{ id: string; name: string }>> {
  const names = new Map(snapshot.run.artifacts.map(artifact => [artifact.artifact_id, artifact.name]));
  const result = new Map<string, Array<{ id: string; name: string }>>();
  for (const event of snapshot.events) {
    if (event.event_type !== 'tool_completed') continue;
    const ids = asObject(event.payload).artifact_ids;
    if (!Array.isArray(ids)) continue;
    const bucket = result.get(event.step_id) || [];
    for (const raw of ids) {
      const id = asText(raw);
      if (!id || bucket.some(item => item.id === id)) continue;
      bucket.push({ id, name: names.get(id) || id });
    }
    result.set(event.step_id, bucket);
  }
  return result;
}

export function buildRunTimeline(snapshot: WorkflowRunSnapshot, nowMs: number): RunTimeline {
  const now = Math.floor(nowMs / 1000);
  const run = snapshot.run;
  const active = ACTIVE_STATUSES.has(run.status);
  const startedAt = toEpochSeconds(run.created_at);
  const finishedAt = toEpochSeconds(run.updated_at);
  const artifacts = artifactsByStep(snapshot);
  const packageSteps = snapshot.workflow?.package.steps || [];

  const steps: RunTimelineStep[] = packageSteps.map(definition => {
    const runtimeStep = run.steps.find(item => item.step_id === definition.id);
    const rawStatus = runtimeStep?.status || (run.status === 'succeeded' ? 'succeeded' : 'pending');
    const state = stepState(rawStatus, run.status);
    const started = toEpochSeconds(runtimeStep?.started_at);
    const finished = toEpochSeconds(runtimeStep?.finished_at);
    const running = state === 'active' || state === 'waiting';
    const duration = finished && started
      ? Math.max(0, finished - started)
      : running && started
        ? Math.max(0, now - started)
        : 0;
    return {
      id: definition.id,
      title: definition.title || definition.id,
      kind: definition.kind || '',
      capabilityId: definition.capability_id || '',
      executionMode: definition.execution_mode || '',
      state,
      durationSeconds: duration,
      running,
      error: runtimeStep?.error || '',
      artifacts: artifacts.get(definition.id) || [],
      startedAt: started,
      attempt: runtimeStep?.attempt || 0,
    };
  });

  const total = steps.length;
  const completed = steps.filter(step => step.state === 'done' || step.state === 'skipped').length;
  const current = steps.find(step => step.state === 'active' || step.state === 'waiting')
    || steps.find(step => step.id === run.current_step_id)
    || null;
  const lastEvent = snapshot.events.reduce((latest, event) => Math.max(latest, toEpochSeconds(event.occurred_at)), 0);
  const lastActivity = Math.max(lastEvent, finishedAt, startedAt);
  const elapsed = active
    ? Math.max(0, (now || startedAt) - startedAt)
    : startedAt && finishedAt
      ? Math.max(0, finishedAt - startedAt)
      : 0;
  const idle = active && lastActivity ? Math.max(0, now - lastActivity) : 0;
  const failedStep = steps.find(step => step.state === 'failed');

  let headline: string;
  if (failedStep) headline = `${failedStep.title} 失败`;
  else if (run.status === 'succeeded') headline = `${total} 步全部完成`;
  else if (run.status === 'canceled') headline = '运行已取消';
  else if (current?.state === 'waiting') headline = `等待处理：${current.title}`;
  else if (current) headline = `正在执行：${current.title}`;
  else if (run.status === 'queued') headline = '已排队，等待开始执行';
  else headline = '准备中';

  return {
    steps,
    completed,
    total,
    percent: total ? Math.round((completed / total) * 100) : 0,
    current,
    active,
    elapsedSeconds: elapsed,
    idleSeconds: idle,
    startedAt,
    headline,
  };
}

/** 事件 → 中文动态。只摘取小字段，禁止把 Step 输出整段带进 UI。 */
export function buildRunActivity(snapshot: WorkflowRunSnapshot): RunActivityItem[] {
  const titleById = new Map((snapshot.workflow?.package.steps || []).map(step => [step.id, step.title || step.id]));
  const items: RunActivityItem[] = [];
  const activeRequest = snapshot.interaction_request;
  const activeRequestEventId = activeRequest?.status === 'pending' ? activeRequest.source_event_id : undefined;

  // 当前等待态使用稳定的 InteractionRequest 投影，避免页面再从事件 payload 猜测
  // 提示文案和动作。原始事件仍保留为历史事实，但不会和当前请求重复一行。
  const requestItem = interactionActivity(snapshot);
  if (requestItem) items.push(requestItem);

  for (const event of snapshot.events) {
    if (
      activeRequestEventId
      && event.event_id === activeRequestEventId
      && (event.event_type === 'question_requested' || event.event_type === 'approval_requested')
    ) continue;
    const payload = asObject(event.payload);
    const output = asObject(payload.output);
    const stepTitle = titleById.get(event.step_id) || event.step_id || '运行';
    const at = toEpochSeconds(event.occurred_at);
    const key = event.event_id || `${event.step_id}:${event.sequence}:${event.event_type}`;
    const artifactIds = Array.isArray(payload.artifact_ids) ? payload.artifact_ids : [];

    switch (event.event_type) {
      case 'tool_started':
        items.push({
          key, at, clock: formatClock(at), stepTitle, kind: 'started',
          text: `${stepTitle} 开始执行`,
          detail: '',
        });
        break;
      case 'tool_completed': {
        const duration = payload.duration_ms || output.duration_ms;
        const parts: string[] = [];
        if (asNumber(duration) > 0) parts.push(`耗时 ${formatElapsedCn(asNumber(duration) / 1000)}`);
        if (artifactIds.length) parts.push(`生成 ${artifactIds.length} 个输出文件`);
        const model = asText(output.model);
        if (model) parts.push(`模型 ${model}`);
        if (output.degraded === true) parts.push('已降级');
        const summary = asText(output.summary);
        items.push({
          key, at, clock: formatClock(at), stepTitle, kind: 'completed',
          text: `${stepTitle} 完成`,
          detail: parts.length ? parts.join(' · ') : truncate(summary, 90),
        });
        break;
      }
      case 'progress': {
        items.push({
          key, at, clock: formatClock(at), stepTitle, kind: 'progress',
          text: `${stepTitle} 进度更新`,
          detail: truncate(asText(payload.message), 90),
        });
        break;
      }
      case 'error':
        items.push({
          key, at, clock: formatClock(at), stepTitle, kind: 'error',
          text: `${stepTitle} 失败`,
          detail: truncate(asText(payload.error) || asText(payload.message), 160),
        });
        break;
      case 'question_requested':
        items.push({
          key, at, clock: formatClock(at), stepTitle, kind: 'question',
          text: `${stepTitle} 请求你的反馈`,
          detail: truncate(asText(payload.question) || asText(payload.message) || asText(payload.reason), 120),
        });
        break;
      case 'approval_requested':
      case 'approval_decided':
        items.push({
          key, at, clock: formatClock(at), stepTitle, kind: 'approval',
          text: event.event_type === 'approval_requested' ? `${stepTitle} 等待审批` : `${stepTitle} 审批已处理`,
          detail: '',
        });
        break;
      default:
        items.push({
          key, at, clock: formatClock(at), stepTitle, kind: 'info',
          text: `${stepTitle} 状态已更新`,
          detail: '',
        });
    }
  }

  // 新的在上：动态流的第一行永远回答「刚刚发生了什么」。
  return items.sort((left, right) => right.at - left.at || right.key.localeCompare(left.key));
}
