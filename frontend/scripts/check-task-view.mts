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

console.log('task view checks passed');
