import type { WorkflowCondition, WorkflowPackage, WorkflowStep } from '../services/agentApi';

/**
 * 将 Workflow Package 的声明式 DAG 投影成适合 UI 展示的小模型。
 * 这里不复制 Runner 状态：它只解释“包声明的执行链路”，运行态仍由 Run View Model 提供。
 */
export type WorkflowGraphNode = {
  id: string;
  stepId: string;
  title: string;
  kind: string;
  approvalRequired: boolean;
  onFailure: string;
  depth: number;
  dependsOn: string[];
  downstream: string[];
  inputArtifacts: string[];
  producedArtifacts: string[];
  condition: string;
  failureCondition: string;
  loop: { maxIterations: number; pauseForFeedback: boolean } | null;
};

export type WorkflowGraph = {
  nodes: WorkflowGraphNode[];
  entrypoints: Array<{ id: string; label: string; atStep: string; produces: string[] }>;
  exits: Array<{ id: string; label: string; atStep: string; produces: string[] }>;
};

export function conditionSummary(condition?: WorkflowCondition | null): string {
  if (!condition) return '';
  const operator = condition.operator || 'condition';
  if (condition.conditions?.length) {
    const joiner = operator === 'all' ? ' 且 ' : operator === 'any' ? ' 或 ' : '、';
    return `${operator}(${condition.conditions.map(conditionSummary).filter(Boolean).join(joiner)})`;
  }
  const path = condition.path?.trim() || '';
  const value = condition.value === undefined || condition.value === null ? '' : ` ${JSON.stringify(condition.value)}`;
  return [operator, path, value].filter(Boolean).join(' ').trim();
}

function stepArtifacts(step: WorkflowStep) {
  return step.runtime?.input_artifacts || [];
}

export function buildWorkflowGraph(pkg: WorkflowPackage): WorkflowGraph {
  const nodes: WorkflowGraphNode[] = [];
  const producedByStep = new Map<string, string[]>();
  const endpoints = [...(pkg.entrypoints || []), ...(pkg.exits || [])];
  for (const endpoint of endpoints) {
    const bucket = producedByStep.get(endpoint.at_step) || [];
    bucket.push(...(endpoint.produces || []));
    producedByStep.set(endpoint.at_step, Array.from(new Set(bucket)));
  }

  function walk(steps: WorkflowStep[], depth: number, parentPrefix = '') {
    for (const step of steps) {
      const id = parentPrefix ? `${parentPrefix}/${step.id}` : step.id;
      const dependsOn = (step.depends_on || []).map(dependency => parentPrefix ? `${parentPrefix}/${dependency}` : dependency);
      nodes.push({
        id,
        stepId: step.id,
        title: step.title || step.id,
        kind: step.kind || 'provider_defined',
        approvalRequired: Boolean(step.approval_required),
        onFailure: step.on_failure || 'fail',
        depth,
        dependsOn,
        downstream: [],
        inputArtifacts: stepArtifacts(step),
        producedArtifacts: producedByStep.get(step.id) || [],
        condition: conditionSummary(step.when),
        failureCondition: conditionSummary(step.fail_when),
        loop: step.loop ? { maxIterations: step.loop.max_iterations, pauseForFeedback: Boolean(step.loop.pause_for_feedback) } : null,
      });
      if (step.loop?.steps?.length) walk(step.loop.steps, depth + 1, id);
    }
  }
  walk(pkg.steps || [], 0);

  const known = new Set(nodes.map(node => node.id));
  for (const node of nodes) {
    for (const dependency of node.dependsOn) {
      const upstream = nodes.find(candidate => candidate.id === dependency);
      if (upstream && !upstream.downstream.includes(node.id)) upstream.downstream.push(node.id);
    }
  }
  // 旧包可能没有声明 depends_on：用声明顺序补一条轻量视觉链，避免 UI 看起来像一堆无关卡片。
  for (let index = 1; index < nodes.length; index += 1) {
    const current = nodes[index];
    if (current.depth !== nodes[index - 1].depth || current.dependsOn.length) continue;
    const previous = nodes[index - 1];
    if (!known.has(previous.id) || previous.id === current.id) continue;
    current.dependsOn.push(previous.id);
    previous.downstream.push(current.id);
  }

  return {
    nodes,
    entrypoints: (pkg.entrypoints || []).map(endpoint => ({
      id: endpoint.id,
      label: endpoint.label || endpoint.id,
      atStep: endpoint.at_step,
      produces: endpoint.produces || [],
    })),
    exits: (pkg.exits || []).map(endpoint => ({
      id: endpoint.id,
      label: endpoint.label || endpoint.id,
      atStep: endpoint.at_step,
      produces: endpoint.produces || [],
    })),
  };
}
