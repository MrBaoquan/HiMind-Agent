import { CircleAlert, Info } from 'lucide-react';
import type { OperationPlan, PlanDependency, PlanStep, PlanTarget } from '../services/agentApi';
import {
  dependencyActionLabel,
  dependencyActionTone,
  dependencyNeedsWork,
  scopeLabel,
  strategyLabel,
} from './operationPlanText';

/**
 * 统一计划卡：安装与发布共用一套渲染。
 *
 * 组件的职责只是"把后端已经算好的结论说清楚"：会写到哪里、怎么写、按什么
 * 顺序做、为什么现在不能做。它不做任何判断，`ready` 与 `blocked_reasons` 都
 * 由后端给出，避免界面和执行面各有各的口径。
 */

export function PlanTargetRow({ target }: { target: PlanTarget }) {
  return <div className="plan-target-row" title={target.destination}>
    <span className="plan-target-name">{target.label}</span>
    <span className="plan-tag">{scopeLabel(target.scope)}</span>
    <span className="plan-tag">{strategyLabel(target.strategy)}</span>
    <code className="plan-target-path">{target.destination || '—'}</code>
  </div>;
}

function PlanDependencyRow({ dependency }: { dependency: PlanDependency }) {
  // 颜色跟着状态走：已就绪不提醒，需要处理给出底色，被阻断用危险色，避免
  // 「能不能装」和「要不要动手」被同一片黄色混在一起。
  const tone = dependencyActionTone(dependency.action);
  return <div className={`plan-dependency-row ${tone}`} title={dependency.reason || undefined}>
    <span className={`status-dot ${tone === 'ready' ? 'success' : tone === 'blocked' ? 'danger' : ''}`} />
    <span className="plan-dependency-name">{dependency.name || dependency.id}</span>
    <span className="plan-tag">{dependency.required ? '必需' : '可选'}</span>
    <span className="plan-dependency-action">{dependencyActionLabel(dependency.action)}</span>
    <code>{dependency.target_version ? `v${dependency.target_version}` : '—'}</code>
  </div>;
}

function PlanStepRow({ step }: { step: PlanStep }) {
  return <div className={`plan-step-row ${step.mutating ? '' : 'readonly'}`}>
    <span className="status-dot" />
    <span className="plan-step-copy"><strong>{step.title}</strong><small>{step.detail}</small></span>
  </div>;
}

export function OperationPlanCard({ plan, heading, showDependencies = true, showSteps = true, showTargets = true }: {
  plan: OperationPlan;
  /** 「会发生什么」这类小节标题，按操作类型换词。 */
  heading?: string;
  showDependencies?: boolean;
  showSteps?: boolean;
  showTargets?: boolean;
}) {
  const targets = showTargets ? plan.targets : [];
  const dependencies = showDependencies ? plan.dependencies : [];
  const steps = showSteps ? plan.steps : [];
  const pendingDependencies = dependencies.filter(dependency => dependencyNeedsWork(dependency.action)).length;
  const created = targets.filter(target => !target.detected).length;
  return <section className="plan-card">
    {/* 落点计数只在列表真的列出来时才有意义：调用方隐藏了「写入位置」时
        （例如技能弹窗另有「安装位置 / 投放目标」），这里就别再报一个空数字。 */}
    {heading ? <div className="plan-card-head"><strong>{heading}</strong>{showTargets ? <small>{created ? `${targets.length} 个落点，其中 ${created} 个尚不存在` : `${targets.length} 个落点`}</small> : null}</div> : null}
    {plan.blocked_reasons.map(reason => <div className="plan-line danger" key={`blocked-${reason}`}><CircleAlert size={14} /><span>{reason}</span></div>)}
    {plan.warnings.map(warning => <div className="plan-line warn" key={`warning-${warning}`}><Info size={14} /><span>{warning}</span></div>)}
    {targets.length ? <div className="plan-block">
      <div className="plan-block-head"><strong>写入位置</strong><small>{plan.operation === 'publish' ? '发布会落到这些目标' : '按这些策略落盘'}</small></div>
      <div className="plan-target-list">{targets.map(target => <PlanTargetRow key={`${target.kind}:${target.id}`} target={target} />)}</div>
    </div> : null}
    {dependencies.length ? <div className="plan-block">
      <div className="plan-block-head"><strong>依赖</strong><small>{pendingDependencies ? `${pendingDependencies} 项需要处理` : '全部已就绪'}</small></div>
      <div className="plan-dependency-list">{dependencies.map(dependency => <PlanDependencyRow key={`${dependency.kind}:${dependency.id}`} dependency={dependency} />)}</div>
    </div> : null}
    {steps.length ? <div className="plan-block">
      <div className="plan-block-head"><strong>执行步骤</strong><small>只读步骤不会改动本机</small></div>
      <div className="plan-step-list">{steps.map(step => <PlanStepRow key={step.id} step={step} />)}</div>
    </div> : null}
  </section>;
}
