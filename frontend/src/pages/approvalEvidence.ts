/**
 * 审批卡片需要的依据。
 *
 * 审批是「用户替系统承担风险」的动作，卡片必须能回答三件事：
 * 改了什么、验证过什么、各方结论是什么。数据全部来自运行快照，
 * 这里只做聚合，不新增采集。
 */
import type { WorkflowRunSnapshot, WorkflowStep } from '../services/agentApi';
import { roleLabel, taskStatusLabel, taskStatusTone, type TaskTone } from './taskView.ts';
import { toEpochSeconds, type RunTimeline } from './workflowRunView.ts';

export type ApprovalEvidenceItem = {
  /** 分组：变更 / 验证 / 结论 */
  group: 'change' | 'verify' | 'verdict';
  title: string;
  detail: string;
  /** 结论类条目带状态色，其余为 neutral。 */
  tone: TaskTone;
};

export type ApprovalEvidence = {
  items: ApprovalEvidenceItem[];
  /** 步骤标题，用于卡片主标题。 */
  stepTitle: string;
  /** 风险等级原值，例如 R4。 */
  risk: string;
  /** 分支数量，0 表示不是委派步骤。 */
  branchCount: number;
};

type SnapshotEvent = WorkflowRunSnapshot['events'][number];

const EVENT_LABELS: Record<string, string> = {
  tool_completed: '完成',
  tool_started: '开始',
  error: '失败',
  approval_requested: '请求审批',
  question_requested: '请求反馈',
};

/** 从事件 payload 里取一段可读摘要，绝不原样贴大对象。 */
function summarizePayload(payload: unknown): string {
  if (!payload || typeof payload !== 'object') return '';
  const record = payload as Record<string, unknown>;
  if (typeof record.error === 'string' && record.error.trim()) return record.error.trim();
  if (typeof record.summary === 'string' && record.summary.trim()) return record.summary.trim();
  if (typeof record.message === 'string' && record.message.trim()) return record.message.trim();
  const output = record.output;
  if (output && typeof output === 'object') {
    const summary = (output as Record<string, unknown>).summary;
    if (typeof summary === 'string' && summary.trim()) return summary.trim();
    const passed = (output as Record<string, unknown>).passed;
    if (typeof passed === 'boolean') return passed ? '步骤报告通过' : '步骤报告未通过';
  }
  return '';
}

/**
 * 聚合审批依据。
 *
 * 刻意只输出「有一条算一条」的条目：没数据的分组整块不出现，
 * 而不是渲染一个「暂无」占位——那会让卡片看起来很满但其实什么都没说。
 */
export function buildApprovalEvidence(
  snapshot: WorkflowRunSnapshot | null,
  timeline: RunTimeline | null,
): ApprovalEvidence {
  if (!snapshot) {
    return { items: [], stepTitle: '', risk: '', branchCount: 0 };
  }
  const run = snapshot.run;
  const stepId = run.current_step_id;
  const current = timeline?.steps.find(step => step.stepId === stepId || step.id === stepId) || null;
  const items: ApprovalEvidenceItem[] = [];

  // 1) 变更：产物数量与文件清单。产物是「这次运行到底产出了什么」最直接的证据。
  const artifacts = current?.artifacts ?? [];
  if (artifacts.length) {
    items.push({
      group: 'change',
      title: `产出 ${artifacts.length} 个文件`,
      detail: artifacts.map(artifact => artifact.name).join('、'),
      tone: 'neutral',
    });
  }

  // 2) 验证：已落定步骤的状态计数。审批前用户最想知道「前面都过了吗」。
  if (timeline) {
    const done = timeline.steps.filter(step => step.state === 'done' || step.state === 'skipped').length;
    if (timeline.total > 0) {
      items.push({
        group: 'verify',
        title: `已落定 ${done}/${timeline.total} 步`,
        detail: timeline.steps
          .filter(step => step.state === 'failed')
          .map(step => `${step.title} 失败：${step.error || '无错误信息'}`)
          .join('；') || '没有失败步骤',
        tone: timeline.steps.some(step => step.state === 'failed') ? 'danger' : 'success',
      });
    }
  }

  // 3) 结论：委派分支各自的产出。多角色协作里，这是「各方怎么说」的唯一来源。
  const branchEvents = snapshot.events.filter(
    (event: SnapshotEvent) => event.step_id === stepId && event.event_type === 'tool_completed',
  );
  const branchLines: string[] = [];
  for (const event of branchEvents) {
    const payload = event.payload as Record<string, unknown> | null;
    const output = payload?.output as Record<string, unknown> | undefined;
    const branches = output?.branches;
    if (!Array.isArray(branches)) continue;
    for (const branch of branches) {
      if (!branch || typeof branch !== 'object') continue;
      const record = branch as Record<string, unknown>;
      const roleId = typeof record.role_id === 'string' ? record.role_id : '';
      const status = typeof record.status === 'string' ? record.status : '';
      const name = roleId ? roleLabel(roleId) : String(record.branch_id ?? '分支');
      const summary = summarizePayload(record.output);
      branchLines.push(
        `${name} · ${taskStatusLabel(status === 'succeeded' ? 'completed' : status) || status}${summary ? `：${summary}` : ''}`,
      );
    }
  }
  if (branchLines.length) {
    items.push({
      group: 'verdict',
      title: `${branchLines.length} 条分支结论`,
      detail: branchLines.join('；'),
      tone: 'neutral',
    });
  }

  // 4) 结论兜底：当前步骤最近一条带摘要的事件。
  if (!branchLines.length) {
    const latest = [...snapshot.events]
      .reverse()
      .find(event => event.step_id === stepId && summarizePayload(event.payload));
    if (latest) {
      const summary = summarizePayload(latest.payload);
      if (summary) {
        items.push({
          group: 'verdict',
          title: `最近事件：${EVENT_LABELS[latest.event_type] || latest.event_type}`,
          detail: summary,
          tone: taskStatusTone(latest.event_type === 'error' ? 'failed' : 'running'),
        });
      }
    }
  }

  // 5) 耗时：审批决策的一个现实依据——跑了多久还没结论。
  if (current && current.durationSeconds > 0) {
    const minutes = Math.round(current.durationSeconds / 60);
    items.push({
      group: 'verify',
      title: '当前步骤耗时',
      detail: minutes >= 1 ? `${minutes} 分钟` : `${current.durationSeconds} 秒`,
      tone: 'neutral',
    });
  }

  return {
    items,
    stepTitle: current?.title || stepId,
    risk: snapshot.workflow?.package?.steps
      ? findStepRisk(snapshot.workflow.package.steps, stepId)
      : '',
    branchCount: branchLines.length,
  };
}

function findStepRisk(steps: WorkflowStep[], stepId: string): string {
  for (const step of steps || []) {
    if (step.id === stepId) return String(step.risk_level || '');
    const nested = step.loop?.steps;
    if (Array.isArray(nested)) {
      const found = findStepRisk(nested, stepId);
      if (found) return found;
    }
    const branches = step.delegate?.branches;
    if (Array.isArray(branches) && branches.some(branch => branch.id === stepId)) {
      return String(step.risk_level || '');
    }
  }
  return '';
}

/** 最近一次事件的时间戳，用于显示「多久没动静」。 */
export function lastEventAt(snapshot: WorkflowRunSnapshot | null): number {
  if (!snapshot) return 0;
  return snapshot.events.reduce(
    (latest, event) => Math.max(latest, toEpochSeconds(event.occurred_at)),
    0,
  );
}
