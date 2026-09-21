import { buildWorkflowGraph, conditionSummary } from '../src/pages/workflowGraph.ts';

const graph = buildWorkflowGraph({
  schema_version: 'workflow_package.v1',
  id: 'fixture',
  version: '1.0.0',
  name: 'fixture',
  description: '',
  min_agent_version: '0.1.0',
  capabilities: [],
  dependencies: { skills: [], plugins: [], connectors: [], runtimes: [] },
  steps: [
    { id: 'prepare', title: '准备', kind: 'capability', capability_id: 'fs.read', execution_mode: 'sync', risk_level: '', approval_required: false, depends_on: [] },
    { id: 'loop', title: '迭代', kind: 'loop', capability_id: '', execution_mode: 'sync', risk_level: '', approval_required: false, depends_on: ['prepare'], loop: { max_iterations: 3, pause_for_feedback: true, steps: [{ id: 'body', title: '执行', kind: 'runtime', capability_id: '', execution_mode: 'sync', risk_level: '', approval_required: false, depends_on: [], runtime: { input_artifacts: ['candidate'] } }] } },
  ],
  artifacts: [],
  ui: { mode: 'standard', entry: '', surfaces: [] },
  entrypoints: [{ id: 'default', at_step: 'prepare', label: '默认入口', produces: ['candidate'] }],
  exits: [{ id: 'done', at_step: 'loop', label: '完成', produces: ['report'] }],
  supported_runtimes: [],
});

if (graph.nodes.length !== 3) throw new Error(`expected 3 graph nodes, got ${graph.nodes.length}`);
if (!graph.nodes.find(node => node.stepId === 'loop')?.loop?.pauseForFeedback) throw new Error('loop metadata was not projected');
if (!graph.nodes.find(node => node.stepId === 'body')?.inputArtifacts.includes('candidate')) throw new Error('artifact input was not projected');
if (graph.entrypoints[0]?.produces[0] !== 'candidate' || graph.exits[0]?.atStep !== 'loop') throw new Error('endpoint metadata was not projected');
if (!conditionSummary({ operator: 'equals', path: 'status', value: 'ok' }).includes('status')) throw new Error('condition summary failed');

console.log('workflow graph checks passed');

