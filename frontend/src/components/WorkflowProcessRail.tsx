import type { CSSProperties } from 'react';
import {
  CircleAlert as circleAlertIcon,
  CircleCheck as circleCheckIcon,
  CircleDashed as circleDashedIcon,
  CircleX as circleXIcon,
  Clock as clockIcon,
  LoaderCircle as loaderCircleIcon,
} from 'lucide';
import { ChevronDown, FileText, GitBranch, Repeat2, ShieldCheck } from 'lucide-react';
import { MorphIcon } from './MorphIcon';
import { Pill } from './Common';
import type { RunActivityItem, RunStepState, RunTimelineStep } from '../pages/workflowRunView.ts';
import type { WorkflowGraphNode } from '../pages/workflowGraph.ts';
import { formatElapsedCn } from '../pages/workflowRunView.ts';

export type WorkflowProcessNode = WorkflowGraphNode | RunTimelineStep;

export function runStepStateLabel(state: RunStepState) {
  return ({
    done: '已完成',
    active: '进行中',
    waiting: '等待处理',
    failed: '失败',
    canceled: '已取消',
    skipped: '已跳过',
    pending: '待执行',
  } as Record<RunStepState, string>)[state] || state;
}

export function runStepStateKind(state: RunStepState): 'success' | 'warn' | 'danger' | 'neutral' {
  if (state === 'done') return 'success';
  if (state === 'failed' || state === 'canceled') return 'danger';
  if (state === 'active' || state === 'waiting') return 'warn';
  return 'neutral';
}

function stateIcon(state: RunStepState) {
  if (state === 'done') return circleCheckIcon;
  if (state === 'active') return loaderCircleIcon;
  if (state === 'waiting') return clockIcon;
  if (state === 'failed' || state === 'canceled') return circleXIcon;
  return circleDashedIcon;
}

function stepKindLabel(kind?: string) {
  return ({
    capability: '自动步骤',
    runtime: 'AI 步骤',
    manual: '人工步骤',
    loop: '循环步骤',
    provider_defined: '扩展步骤',
  } as Record<string, string>)[kind || ''] || '工作流步骤';
}

function isRunStep(node: WorkflowProcessNode): node is RunTimelineStep {
  return 'state' in node;
}

/**
 * 定义和运行共用的一条过程轨道。
 * 定义态只解释结构；运行态在同一个节点上叠加状态、耗时和当前步骤事件，避免另起一套列表。
 */
export function WorkflowProcessRail({ nodes, mode, activities = [] }: {
  nodes: WorkflowProcessNode[];
  mode: 'definition' | 'run';
  activities?: RunActivityItem[];
}) {
  return (
    <div className={`workflow-process-rail ${mode}`} role={mode === 'run' ? 'list' : undefined} aria-label={mode === 'run' ? '执行步骤' : '工作流执行结构'}>
      {nodes.map((node, index) => {
        const runStep = isRunStep(node) ? node : null;
        const state = runStep?.state || 'pending';
        const nodeActivities = runStep
          ? activities.filter(item => item.stepId === node.stepId || item.stepId === node.id)
          : [];
        const latestActivity = nodeActivities[0];
        const showLatestActivity = Boolean(runStep && ['active', 'waiting', 'failed'].includes(runStep.state));
        const artifactCount = isRunStep(node) ? node.artifacts.length : node.producedArtifacts.length;
        const style = { '--process-depth': Math.min(node.depth, 3) } as CSSProperties;
        return (
          <div
            key={node.id}
            className={[
              'workflow-process-node',
              runStep ? `is-${state}` : '',
              runStep?.running ? 'is-running' : '',
            ].filter(Boolean).join(' ')}
            style={style}
            role={mode === 'run' ? 'listitem' : undefined}
            aria-current={runStep && (state === 'active' || state === 'waiting') ? 'step' : undefined}
            data-step-id={node.stepId}
          >
            <div className="workflow-process-node-card">
              <span className={`workflow-process-node-state ${runStep ? `is-${state}` : 'is-definition'}${runStep?.state === 'active' ? ' is-spinning' : ''}`} aria-hidden="true">
                <MorphIcon icon={runStep ? stateIcon(state) : circleDashedIcon} size={16} strokeWidth={2} />
              </span>
              <span className="workflow-process-node-main">
                <span className="workflow-process-node-title">
                  <b className="workflow-process-node-index">{String(index + 1).padStart(2, '0')}</b>
                  <strong title={node.title}>{node.title}</strong>
                  {node.approvalRequired ? <ShieldCheck size={13} aria-label="需要审批" /> : null}
                  {node.loop ? <Repeat2 size={12} aria-label="循环步骤" /> : null}
                </span>
                <small className="workflow-process-node-meta">
                  {[stepKindLabel(node.kind), runStep && runStep.durationSeconds > 0 ? `耗时 ${formatElapsedCn(runStep.durationSeconds)}` : '', runStep?.attempt && runStep.attempt > 1 ? `第 ${runStep.attempt} 次尝试` : ''].filter(Boolean).join(' · ')}
                </small>
                <span className="workflow-process-node-tags">
                  {node.condition ? <b><GitBranch size={11} />条件：{node.condition}</b> : null}
                  {node.loop ? <b><Repeat2 size={11} />最多 {node.loop.maxIterations} 轮{node.loop.pauseForFeedback ? ' · 每轮可反馈' : ''}</b> : null}
                  {node.onFailure === 'continue' ? <b>失败后继续</b> : null}
                  {artifactCount ? <b><FileText size={11} />产出 {artifactCount} 个文件</b> : null}
                </span>
                {runStep?.error ? <small className="workflow-process-node-error">{runStep.error}</small> : null}
                {showLatestActivity && latestActivity ? (
                  <small className="workflow-process-latest-event">
                    <time>{latestActivity.clock}</time>
                    <span>{latestActivity.text}{latestActivity.detail ? ` · ${latestActivity.detail}` : ''}</span>
                  </small>
                ) : null}
              </span>
              {runStep ? (
                <Pill kind={runStepStateKind(state)}>
                  {runStep.running && state === 'active' ? `进行中 ${formatElapsedCn(runStep.durationSeconds)}` : runStepStateLabel(state)}
                </Pill>
              ) : null}
            </div>
            {runStep && nodeActivities.length ? (
              <details className="workflow-process-events" open={state === 'failed' ? true : undefined}>
                <summary>
                  <span>事件 {nodeActivities.length} 条</span>
                  <ChevronDown size={13} aria-hidden="true" />
                </summary>
                <div className="workflow-process-event-list">
                  {nodeActivities.map(item => (
                    <div key={item.key} className={item.kind}>
                      <time>{item.clock}</time>
                      <span><strong>{item.text}</strong>{item.detail ? <small>{item.detail}</small> : null}</span>
                    </div>
                  ))}
                </div>
              </details>
            ) : null}
          </div>
        );
      })}
    </div>
  );
}
