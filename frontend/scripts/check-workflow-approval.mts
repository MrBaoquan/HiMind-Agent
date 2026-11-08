// 审批依据聚合的自检（零依赖：node --experimental-strip-types）。
//
// 审批卡片要回答「改了什么 / 验证过什么 / 各方结论是什么」。这里锁住三件事：
//   1. 没数据时整块不渲染（不留「暂无」占位）
//   2. 有委派分支时，每条分支都出现在结论里
//   3. 有失败步骤时，验证分组带 danger 且写出失败原因
import { strict as assert } from 'node:assert';
import { buildApprovalEvidence } from '../src/pages/approvalEvidence.ts';

const packageStub = {
  steps: [
    { id: 'CREATE-PR', title: '创建 PR', risk_level: 'R4' },
    { id: 'AUDIT', title: '审查', risk_level: 'read_only', delegate: { branches: [{ id: 'IA' }, { id: 'CRAFT' }] } },
  ],
};

function snapshot(overrides: Record<string, unknown> = {}) {
  return {
    run: {
      run_id: 'workflow_run_1',
      status: 'waiting',
      current_step_id: 'CREATE-PR',
      steps: [],
      artifacts: [],
    },
    events: [],
    workflow: { package: { ...packageStub, name: 'x', id: 'x', version: '1.0.0' } },
    ...overrides,
  } as any;
}

function timeline(steps: any[]) {
  return {
    steps,
    total: steps.length,
    completed: steps.filter(s => s.state === 'done').length,
    active: false,
    elapsedSeconds: 0,
    idleSeconds: 0,
    startedAt: 0,
    percent: 0,
    current: null,
    headline: '',
  } as any;
}

// 1. 空数据：不给任何条目，卡片不渲染证据块。
{
  const evidence = buildApprovalEvidence(snapshot(), timeline([]));
  assert.equal(evidence.items.length, 0, '无数据时不得产出条目');
  assert.equal(evidence.stepTitle, 'CREATE-PR');
  assert.equal(evidence.risk, 'R4', '风险等级应来自包定义');
}

// 2. 产物 + 失败步骤：变更与验证各一条，失败带 danger。
{
  const evidence = buildApprovalEvidence(
    snapshot(),
    timeline([
      { stepId: 'AUDIT', id: 'AUDIT', title: '审查', state: 'failed', error: '窄窗溢出', artifacts: [] },
      { stepId: 'CREATE-PR', id: 'CREATE-PR', title: '创建 PR', state: 'waiting', error: '', artifacts: [{ id: 'a', name: 'diff.patch' }], durationSeconds: 90 },
    ]),
  );
  const change = evidence.items.find(item => item.group === 'change');
  assert.ok(change, '有产物时必须给出变更条目');
  assert.match(change!.title, /1 个文件/);
  assert.match(change!.detail, /diff\.patch/);

  const verify = evidence.items.find(item => item.group === 'verify' && item.title.includes('已落定'));
  assert.ok(verify, '有步骤时必须给出落定进度');
  assert.equal(verify!.tone, 'danger', '存在失败步骤时验证分组必须是 danger');
  assert.match(verify!.detail, /窄窗溢出/, '失败原因必须写出来，不能只说「有失败」');
}

// 3. 委派分支：每条分支都进结论，角色名优先于 branch_id。
{
  const evidence = buildApprovalEvidence(
    snapshot({
      events: [
        {
          step_id: 'CREATE-PR',
          event_type: 'tool_completed',
          occurred_at: '100',
          payload: {
            output: {
              branches: [
                { branch_id: 'IA', role_id: 'ui.auditor', status: 'succeeded', output: { summary: '导航层级偏深' } },
                { branch_id: 'CRAFT', role_id: 'ui.craft', status: 'failed', output: null },
              ],
            },
          },
        },
      ],
    }),
    timeline([]),
  );
  const verdict = evidence.items.find(item => item.group === 'verdict');
  assert.ok(verdict, '有委派分支时必须给出结论条目');
  assert.equal(evidence.branchCount, 2);
  assert.match(verdict!.detail, /信息架构审查/);
  assert.match(verdict!.detail, /导航层级偏深/);
  assert.match(verdict!.detail, /工艺审查/);
}

// 4. 无分支时回落到最近一条带摘要的事件，且不伪造结论。
{
  const evidence = buildApprovalEvidence(
    snapshot({
      events: [
        { step_id: 'CREATE-PR', event_type: 'error', occurred_at: '100', payload: { error: '上游步骤未产出版本号' } },
      ],
    }),
    timeline([]),
  );
  const verdict = evidence.items.find(item => item.group === 'verdict');
  assert.ok(verdict, '有带摘要的事件时必须给出结论');
  assert.match(verdict!.detail, /上游步骤未产出版本号/);
  assert.equal(evidence.branchCount, 0);
}

// 5. 委托步骤的 risk 从父步骤取，不从分支取。
{
  const evidence = buildApprovalEvidence(
    snapshot({ run: { run_id: 'r', status: 'waiting', current_step_id: 'IA', steps: [], artifacts: [] } }),
    timeline([]),
  );
  assert.equal(evidence.risk, 'read_only', '分支步骤的 risk 应回落到父步骤');
}

console.log('approval evidence checks passed');
