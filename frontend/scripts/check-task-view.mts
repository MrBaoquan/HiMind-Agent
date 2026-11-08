// 任务中心展示逻辑的回归自检（零依赖：node --experimental-strip-types）。
// 任务中心把「指标卡 + 正在执行 + 记录列表」放在同一页，状态归类和相对时间
// 一旦算错，用户看到的就是错的数字，所以这两块单独锁住。
import { strict as assert } from 'node:assert';
import {
  TASK_ACTIVE_STATUSES,
  TASK_ATTENTION_STATUSES,
  TASK_COMPLETED_STATUSES,
  formatDuration,
  formatRelativeTime,
  taskBucket,
  taskElapsedSeconds,
  taskProgressLabel,
  taskProgressValue,
  taskStatusLabel,
  taskStatusTone,
  taskStepLabel,
  taskTypeLabel,
  shortRunId,
  roleLabel,
  roleShortName,
  parentKindLabel,
  teamSummary,
  roleChips,
  shouldShowRoleBadge,
  shouldShowRoleChain,
  childRuns,
  chainState,
} from '../src/pages/taskView.ts';

// 状态集合必须互斥，否则同一个任务会被同时算进「进行中」和「需处理」。
for (const status of TASK_ACTIVE_STATUSES) {
  assert.ok(!TASK_COMPLETED_STATUSES.has(status), `${status} 不能同时算进行中和已完成`);
  assert.ok(!TASK_ATTENTION_STATUSES.has(status), `${status} 不能同时算进行中和需处理`);
}
for (const status of TASK_COMPLETED_STATUSES) {
  assert.ok(!TASK_ATTENTION_STATUSES.has(status), `${status} 不能同时算已完成和需处理`);
}

assert.equal(taskBucket('running'), 'active');
assert.equal(taskBucket('canceling'), 'active');
assert.equal(taskBucket('completed'), 'completed');
assert.equal(taskBucket('failed'), 'attention');
assert.equal(taskBucket('canceled'), 'attention');
// 本机运行台账的 waiting 必须归到「需处理」，否则等待审批的运行会隐形。
assert.equal(taskBucket('waiting'), 'attention');
assert.ok(!TASK_ACTIVE_STATUSES.has('waiting'));
assert.equal(taskStatusLabel('waiting'), '等待确认');
// 未知状态不能被悄悄算进已完成，否则指标卡会虚高。
assert.equal(taskBucket('queued'), 'unknown');
assert.equal(taskBucket(''), 'unknown');

assert.equal(taskStatusTone('completed'), 'success');
assert.equal(taskStatusTone('failed'), 'danger');
assert.equal(taskStatusTone('canceled'), 'neutral');
assert.equal(taskStatusTone('pending'), 'pending');
assert.equal(taskStatusTone('running'), 'running');
// 跳过的步骤既不是成功也不是在跑：角色链路上必须画成灰色，
// 否则「这一步根本没执行」看起来像「正在做」。
assert.equal(taskStatusTone('skipped'), 'neutral');
assert.equal(taskStatusLabel('skipped'), '已跳过');
assert.equal(taskStatusLabel('canceling'), '取消中');
assert.equal(taskTypeLabel('agent_run'), 'AI 任务');
assert.equal(taskTypeLabel('unknown_type'), 'unknown_type');

// 相对时间按 分钟 / 小时 / 天 / 绝对时间 分桶，边界取整不能漂。
const now = Date.parse('2026-09-21T12:00:00Z');
assert.equal(formatRelativeTime('2026-09-21T11:59:30Z', now), '刚刚');
assert.equal(formatRelativeTime('2026-09-21T11:30:00Z', now), '30 分钟前');
assert.equal(formatRelativeTime('2026-09-21T09:00:00Z', now), '3 小时前');
assert.equal(formatRelativeTime('2026-09-19T12:00:00Z', now), '2 天前');
assert.equal(formatRelativeTime('2026-08-01T12:00:00Z', now).startsWith('2026/'), true);
assert.equal(formatRelativeTime('', now), '--');
// 无法解析的时间原样返回，不伪装成「刚刚」。
assert.equal(formatRelativeTime('not-a-date', now), 'not-a-date');
// 时钟偏差导致的未来时间按「刚刚」处理，不出现负数。
assert.equal(formatRelativeTime('2026-09-21T12:05:00Z', now), '刚刚');

// 不足一秒的取整结果是 0，单独显示 < 1 秒；整一秒就是 1 秒。
assert.equal(formatDuration('2026-09-21T11:59:59.600Z', '2026-09-21T12:00:00Z'), '< 1 秒');
assert.equal(formatDuration('2026-09-21T11:59:59Z', '2026-09-21T12:00:00Z'), '1 秒');
assert.equal(formatDuration('2026-09-21T11:59:00Z', '2026-09-21T12:00:00Z'), '1 分 0 秒');
assert.equal(formatDuration('2026-09-21T11:00:00Z', '2026-09-21T12:30:00Z'), '1 小时 30 分');
// 缺任一端不显示耗时，避免出现 NaN。
assert.equal(formatDuration('2026-09-21T11:00:00Z', null), '');
assert.equal(formatDuration(null, null), '');
assert.equal(formatDuration('bad', '2026-09-21T12:00:00Z'), '');

// 长运行号只留尾号：短号原样显示，长号不能被截成一行看不清的乱码。
assert.equal(shortRunId('run_42'), 'run_42');
assert.equal(shortRunId('run_12345678'), 'run_12345678');
assert.equal(shortRunId(' workflow_run_inv_1790228440476_15:1790228443195-41 '), '#319541');
// 本机运行号尾部的 `_<进程号>` 是排查用的，展示层要剥掉，不能出现 `674_6480` 这种尾巴。
assert.equal(shortRunId('workflow_run_inv_1790232622171_15:1790232624674_6480'), '#624674');
assert.equal(shortRunId(''), '');

// 进度未知必须说「进行中」：显示 0% 会被读成「一步都没动」。
assert.equal(taskProgressValue(null), null);
assert.equal(taskProgressValue(undefined), null);
assert.equal(taskProgressValue(Number.NaN), null);
assert.equal(taskProgressValue(-5), 0);
assert.equal(taskProgressValue(66.6), 67);
assert.equal(taskProgressValue(140), 100);
assert.equal(taskProgressLabel(null), '进行中');
assert.equal(taskProgressLabel(0), '0%');
assert.equal(taskProgressLabel(50), '50%');

// 步骤文案没有总数就不显示，且完成的步数不会超过总数。
assert.equal(taskStepLabel(2, 5), '步骤 2/5');
assert.equal(taskStepLabel(0, 0), '');
assert.equal(taskStepLabel(null, 5), '');
assert.equal(taskStepLabel(2, null), '');
assert.equal(taskStepLabel(9, 5), '步骤 5/5');

// 「已运行」按秒走动，起点缺失或坏值时给 null，界面不显示而不是显示 0 秒。
assert.equal(taskElapsedSeconds('2026-09-21T11:59:00Z', now), 60);
assert.equal(taskElapsedSeconds(null, now), null);
assert.equal(taskElapsedSeconds('bad', now), null);
assert.equal(taskElapsedSeconds('2026-09-21T12:00:30Z', now), 0);

// 角色：已知角色给中文短名，未知角色原样返回 ID——宁可显示英文也不猜错。
assert.equal(roleLabel('software.implementer'), '实现');
assert.equal(roleLabel('software.reviewer'), '代码审查');
assert.equal(roleLabel('software.auditor'), 'software.auditor');
assert.equal(roleLabel(''), '');
assert.equal(roleLabel(null), '');
assert.equal(roleLabel('  software.verifier  '), '测试');
assert.equal(roleShortName('software.implementer'), 'implementer');
assert.equal(roleShortName('solo'), 'solo');
assert.equal(roleShortName(undefined), '');

// 委派与重做是两种不同事实，文案不能合并成一个「子任务」。
assert.equal(parentKindLabel('delegate'), '委派');
assert.equal(parentKindLabel('retry'), '重做');
assert.equal(parentKindLabel(''), '');
assert.equal(parentKindLabel(null), '');
assert.equal(parentKindLabel('something-else'), '');

// 团队计数：缺失或全零时整块不渲染，避免出现一排 0 的空指标。
assert.equal(teamSummary(null), null);
assert.equal(teamSummary(undefined), null);
assert.equal(teamSummary({}), null);
assert.equal(teamSummary({ delegated: 0, retries: 0, running: 0 }), null);
assert.deepEqual(teamSummary({ delegated: 3, retries: 1, running: 2 }), { delegated: 3, retries: 1, running: 2 });
// 脏数据（负数、NaN、字符串）不能被当成计数显示。
assert.equal(teamSummary({ delegated: -1, retries: Number.NaN, running: '2' }), null);
assert.deepEqual(teamSummary({ delegated: 1 }), { delegated: 1, retries: 0, running: 0 });

// 角色链路：保持后端出现顺序（它反映步骤推进），去重，忽略脏数据。
assert.deepEqual(roleChips(null), []);
assert.deepEqual(roleChips(undefined), []);
assert.deepEqual(roleChips('not-an-array'), []);
assert.deepEqual(
  roleChips([
    { id: 'software.analyst', status: 'completed' },
    { id: 'software.implementer', status: 'running' },
    { id: 'software.analyst', status: 'completed' },
    { id: '', status: 'running' },
    { status: 'running' },
    null,
  ]),
  [
    { id: 'software.analyst', label: '需求分析', status: 'completed' },
    { id: 'software.implementer', label: '实现', status: 'running' },
  ],
);
// 未知角色保留原 ID 作为文案，状态缺失给空串，界面据此不套用状态色。
assert.deepEqual(roleChips([{ id: 'custom.role' }]), [{ id: 'custom.role', label: 'custom.role', status: '' }]);

// 什么时候显示角色 UI：无角色不显示徽标，单角色不画链路。
// 这两条决定了旧数据升级后界面纹丝不动，是本次改动最重要的回归面。
assert.equal(shouldShowRoleBadge(''), false);
assert.equal(shouldShowRoleBadge(null), false);
assert.equal(shouldShowRoleBadge('   '), false);
assert.equal(shouldShowRoleBadge('software.verifier'), true);
assert.equal(shouldShowRoleChain([]), false);
assert.equal(shouldShowRoleChain([{ id: 'software.verifier', status: 'running' }]), false);
assert.equal(shouldShowRoleChain([
  { id: 'software.analyst', status: 'completed' },
  { id: 'software.verifier', status: 'running' },
]), true);

// 子运行明细：缺 run_id 的条目不能渲染（点了会打不开），未知 kind 保留原文案。
assert.deepEqual(childRuns(null), []);
assert.deepEqual(childRuns([]), []);
assert.deepEqual(
  childRuns([
    { run_id: 'workflow_run_1', kind: 'delegate', role_id: 'software.verifier', status: 'running' },
    { run_id: 'workflow_run_2', kind: 'retry', status: 'completed' },
    { run_id: '', kind: 'delegate', status: 'running' },
    null,
  ]),
  [
    {
      runId: 'workflow_run_1', kind: 'delegate', kindLabel: '委派',
      roleId: 'software.verifier', roleLabel: '测试',
      status: 'running', statusLabel: '运行中', error: '', updatedAt: '',
    },
    {
      runId: 'workflow_run_2', kind: 'retry', kindLabel: '重做',
      roleId: '', roleLabel: '',
      status: 'completed', statusLabel: '已完成', error: '', updatedAt: '',
    },
  ],
);

// 链路上的状态比徽标多一档：等待审批要单独可辨，不能和「还没轮到」同色。
// 这两者在真实任务里的处置完全不同——前者需要人点一下，后者只是还没开始。
assert.equal(chainState('completed'), 'success');
assert.equal(chainState('succeeded'), 'success');
assert.equal(chainState('running'), 'running');
assert.equal(chainState('waiting'), 'waiting');
assert.equal(chainState('failed'), 'danger');
assert.equal(chainState('canceled'), 'danger');
assert.equal(chainState('pending'), 'pending');
// 后端在投影时就把 LocalStepStatus::Succeeded 归一成 completed，所以链路只会收到
// 归一后的词表；未归一的原始词不需要在这里再认一遍。
assert.equal(chainState('skipped'), 'pending');
assert.equal(chainState(''), 'pending');
// taskStatusTone 把 waiting 归到 neutral 是对的（徽标复用配色），
// 但链路必须区分二者 —— 这条断言锁住两者不相等。
assert.notEqual(chainState('waiting'), taskStatusTone('waiting'));

// ── 行内操作的可判定条件 ─────────────────────────────────────────────────
// 这里锁的是「什么时候按钮才出现」。错误地显示一个点了没反应的按钮，
// 比不显示更糟——用户会以为功能坏了。
import { readFileSync } from 'node:fs';
const page = readFileSync(new URL('../src/pages/TaskCenterPage.tsx', import.meta.url), 'utf8');
assert.match(
  page,
  /const canCancel = Boolean\(onCancelRun\) && active && Boolean\(entry\.workflowRunId\);/,
  '取消只在「有回调 + 正在跑 + 有运行号」时出现',
);
assert.match(
  page,
  /const awaitingApproval = entry\.status === 'waiting' && entry\.pauseKind !== 'user';/,
  '审批入口只认 waiting 且非用户暂停',
);
assert.match(
  page,
  /const canDecide = Boolean\(onApproveRun && onRejectRun\) && awaitingApproval && Boolean\(entry\.workflowRunId\);/,
  '批准/拒绝必须同时具备两个回调',
);
assert.match(
  page,
  /const canPause = Boolean\(onPauseRun\) && entry\.status === 'running' && Boolean\(entry\.workflowRunId\);/,
  '暂停只对正在跑的运行出现：等审批/等反馈不是「在跑」',
);
assert.match(
  page,
  /const canResume = Boolean\(onResumePausedRun\) && entry\.status === 'waiting'\s*&& entry\.pauseKind === 'user'/,
  '恢复只对用户主动暂停出现，不能用来绕过审批或反馈等待',
);
// 等审批的判定必须排除用户暂停，否则暂停中的运行会同时出现「继续」和「批准」。
assert.match(
  page,
  /const awaitingApproval = entry\.status === 'waiting' && entry\.pauseKind !== 'user';/,
  '用户暂停的运行不得出现审批按钮',
);
assert.match(
  page,
  /const canRetry = Boolean\(onRetryStep\) && entry\.status === 'failed'\s*&& Boolean\(entry\.workflowRunId\) && Boolean\(entry\.failedStepId\);/,
  '重试只对失败且已停下的运行出现',
);
// 不允许出现「禁用但可见」的占位按钮。
assert.equal(
  /task-center-row-action[^>]*disabled=\{!on/.test(page),
  false,
  '不得用 disabled 表达「没有权限」，应直接不渲染',
);

console.log('task view checks passed');
