// 运行动态的回归自检（零依赖：node --experimental-strip-types）。
// 用真实事件的形状（含一个 200KB 级的 step 输出）验证两件事：
//   1. 时间线/进度/活性数字算得对；
//   2. 动态流只输出小字段摘要，绝不把 Step 输出贴进界面。
import { strict as assert } from 'node:assert';
import { buildRunActivity, buildRunTimeline } from '../src/pages/workflowRunView.ts';

const packageSteps = [
  { id: 'TR-COLLECT', title: '采集与筛选公开热点', kind: 'capability', capability_id: 'tech-radar.collect', execution_mode: 'long_running', risk_level: 'read_only', approval_required: false, depends_on: [] },
  { id: 'TR-INSIGHT', title: '结合公司上下文生成解读', kind: 'runtime', capability_id: '', execution_mode: 'sync', risk_level: 'read_only', approval_required: false, depends_on: ['TR-COLLECT'] },
  { id: 'TR-REPORT', title: '生成网页报告并归档', kind: 'capability', capability_id: 'tech-radar.report', execution_mode: 'sync', risk_level: 'local_write', approval_required: false, depends_on: ['TR-INSIGHT'] },
];

// 真实事故里最容易被顺手贴进 UI 的东西：快照正文。
const hugeSnapshot = { entries: Array.from({ length: 200 }, (_, index) => ({ entry_id: `owner/repo-${index}`, description: 'x'.repeat(400) })) };

const baseRun = {
  run_id: 'workflow_run_test_0001',
  interaction_id: 'workflow_int_test_0001',
  status: 'succeeded',
  current_step_id: '',
  runtime_provider: 'himind.builtin',
  workspace_ref: '',
  steps: [
    { step_id: 'TR-COLLECT', title: '采集与筛选公开热点', status: 'succeeded', capability_id: 'tech-radar.collect', attempt: 1, started_at: '1789887829', finished_at: '1789887886', error: '' },
    { step_id: 'TR-INSIGHT', title: '结合公司上下文生成解读', status: 'succeeded', capability_id: 'TR-INSIGHT', attempt: 1, started_at: '1789887886', finished_at: '1789887906', error: '' },
    { step_id: 'TR-REPORT', title: '生成网页报告并归档', status: 'succeeded', capability_id: 'tech-radar.report', attempt: 1, started_at: '1789887906', finished_at: '1789887907', error: '' },
  ],
  approvals: [],
  artifacts: [
    { artifact_id: 'tech-radar-snapshot', artifact_type: 'tech_radar_snapshot', name: '热点采集快照 2026-09-20', uri: 'file:///snapshot.json', sha256: 'a'.repeat(64), size_bytes: 0 },
    { artifact_id: 'tech-radar-report', artifact_type: 'tech_radar_report', name: '科技雷达报告 2026-09-20', uri: 'file:///report.json', sha256: 'b'.repeat(64), size_bytes: 0 },
  ],
  error: '',
  created_at: '1789887829',
  updated_at: '1789887907',
};

const snapshot = {
  run: baseRun,
  interaction: {},
  events: [
    { event_id: 'e1', step_id: 'TR-COLLECT', capability_id: 'tech-radar.collect', sequence: 1, occurred_at: '1789887829', provider: 'himind.builtin', event_type: 'tool_started', payload: {} },
    { event_id: 'e2', step_id: 'TR-COLLECT', capability_id: 'tech-radar.collect', sequence: 2, occurred_at: '1789887886', provider: 'himind.builtin', event_type: 'tool_completed', payload: { artifact_ids: ['tech-radar-snapshot'], output: { draft: hugeSnapshot } } },
    { event_id: 'e3', step_id: 'TR-INSIGHT', capability_id: '', sequence: 3, occurred_at: '1789887886', provider: 'himind.builtin', event_type: 'tool_started', payload: {} },
    { event_id: 'e4', step_id: 'TR-INSIGHT', capability_id: '', sequence: 4, occurred_at: '1789887906', provider: 'himind.builtin', event_type: 'tool_completed', payload: { artifact_ids: ['tech-radar-snapshot'], output: { model: 'deepseek-chat', service_source: 'custom', summary: '本期 CV 以底层库与检测框架迭代为主线。' } } },
    { event_id: 'e5', step_id: 'TR-REPORT', capability_id: 'tech-radar.report', sequence: 5, occurred_at: '1789887906', provider: 'himind.builtin', event_type: 'tool_started', payload: {} },
    { event_id: 'e6', step_id: 'TR-REPORT', capability_id: 'tech-radar.report', sequence: 6, occurred_at: '1789887907', provider: 'himind.builtin', event_type: 'tool_completed', payload: { artifact_ids: ['tech-radar-report'], output: {} } },
    { event_id: 'e7', step_id: 'TR-REPORT', capability_id: 'tech-radar.report', sequence: 7, occurred_at: '1789887907', provider: 'himind.builtin', event_type: 'progress', payload: { exitpoint: 'published', completion_mode: 'partial' } },
  ],
  projections: [],
  workflow: { package: { id: 'com.himind.workflow.tech-radar', name: '科技雷达日报', steps: packageSteps }, enabled: true, view: null },
};

// 1) 完成态：整体进度、耗时、每步耗时都要对得上真实事件。
const done = buildRunTimeline(snapshot as never, 1789888000_000);
assert.equal(done.total, 3);
assert.equal(done.completed, 3);
assert.equal(done.percent, 100);
assert.equal(done.active, false);
assert.equal(done.elapsedSeconds, 78);
assert.equal(done.headline, '3 步全部完成');
assert.deepEqual(done.steps.map(step => step.state), ['done', 'done', 'done']);
assert.deepEqual(done.steps.map(step => step.durationSeconds), [57, 20, 1]);
assert.deepEqual(done.steps[0].artifacts.map(artifact => artifact.name), ['热点采集快照 2026-09-20']);

// 2) 运行态：进行中的步骤按「至今」计时，进度是部分完成，并给出空转秒数。
const running = {
  ...snapshot,
  run: {
    ...baseRun,
    status: 'running',
    current_step_id: 'TR-INSIGHT',
    updated_at: '1789887886',
    steps: [
      baseRun.steps[0],
      { ...baseRun.steps[1], status: 'running', finished_at: '' },
      { ...baseRun.steps[2], status: 'pending', started_at: '', finished_at: '' },
    ],
  },
  events: snapshot.events.slice(0, 3),
};
const live = buildRunTimeline(running as never, 1789887916_000);
assert.equal(live.active, true);
assert.equal(live.completed, 1);
assert.equal(live.percent, 33);
assert.equal(live.current?.id, 'TR-INSIGHT');
assert.equal(live.current?.durationSeconds, 30);
assert.equal(live.elapsedSeconds, 87);
assert.equal(live.idleSeconds, 30);
assert.equal(live.headline, '正在执行：结合公司上下文生成解读');

// 3) 失败态：动态与标题都要指向那一步，而不是笼统的「运行失败」。
const failed = {
  ...snapshot,
  run: {
    ...baseRun,
    status: 'failed',
    error: 'capability input property has invalid value: capability input.domains[0]',
    steps: [
      { ...baseRun.steps[0], status: 'failed', error: 'capability input property has invalid value: capability input.domains[0]' },
      { ...baseRun.steps[1], status: 'pending', started_at: '', finished_at: '' },
      { ...baseRun.steps[2], status: 'pending', started_at: '', finished_at: '' },
    ],
  },
  events: [
    snapshot.events[0],
    { event_id: 'e9', step_id: 'TR-COLLECT', capability_id: 'tech-radar.collect', sequence: 2, occurred_at: '1789887830', provider: 'himind.builtin', event_type: 'error', payload: { error: 'capability input property has invalid value: capability input.domains[0]' } },
  ],
};
const broken = buildRunTimeline(failed as never, 1789888000_000);
assert.equal(broken.headline, '采集与筛选公开热点 失败');
assert.equal(broken.steps[0].state, 'failed');
assert.equal(broken.steps[0].error.includes('domains[0]'), true);
assert.equal(broken.percent, 0);

// 4) 动态流：新的在上、事件可读，且巨型 payload 不会渗进界面。
const activity = buildRunActivity(snapshot as never);
assert.equal(activity.length, 7);
assert.equal(activity[0].stepTitle, '生成网页报告并归档');
assert.equal(activity[0].text, '生成网页报告并归档 进度更新');
assert.equal(activity[0].detail, '');
assert.equal(activity[0].detail.includes('published'), false);
assert.equal(activity[0].detail.includes('partial'), false);
assert.equal(activity.find(item => item.key === 'e2')?.detail, '生成 1 个输出文件');
assert.equal(activity.find(item => item.key === 'e4')?.detail, '生成 1 个输出文件 · 模型 deepseek-chat');
for (const item of activity) {
  assert.equal(item.text.length < 80, true, `text too long: ${item.text}`);
  assert.equal(item.detail.length < 200, true, `detail too long: ${item.detail}`);
  assert.equal(item.detail.includes('owner/repo-0'), false, 'payload leaked into activity');
}
const errorItem = buildRunActivity(failed as never).find(item => item.kind === 'error');
assert.equal(errorItem?.text, '采集与筛选公开热点 失败');
assert.equal(errorItem?.detail.includes('domains[0]'), true);

// 5) 等待态优先消费统一 InteractionRequest，并隐藏同一 source event 的重复提示。
const waiting = {
  ...snapshot,
  run: { ...baseRun, status: 'waiting', current_step_id: 'TR-INSIGHT' },
  interaction_request: {
    schema_version: 'interaction_request.v1',
    id: 'request-1',
    run_id: baseRun.run_id,
    step_id: 'TR-INSIGHT',
    kind: 'external_wait',
    title: '等待外部回执',
    description: '请在外部系统完成确认后刷新运行状态。',
    required_action: 'inspect_run',
    status: 'pending',
    source_event_id: 'wait-1',
    created_at: '1789887910',
    schema: { type: 'object' },
  },
  events: [
    ...snapshot.events,
    { event_id: 'wait-1', step_id: 'TR-INSIGHT', capability_id: '', sequence: 8, occurred_at: '1789887910', provider: 'himind.builtin', event_type: 'question_requested', payload: { question: 'legacy payload should not win' } },
  ],
};
const waitingActivity = buildRunActivity(waiting as never);
assert.equal(waitingActivity[0].kind, 'interaction');
assert.equal(waitingActivity[0].detail.includes('外部系统完成确认'), true);
assert.equal(waitingActivity.filter(item => item.key === 'wait-1').length, 0);

console.log('run view checks passed');
