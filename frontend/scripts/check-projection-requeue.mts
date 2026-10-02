// 同步死信恢复的端到端不变量（零依赖：node --experimental-strip-types）。
//
// 这条链路横跨四层，任何一层被改动都可能让它静默失效：前端按钮 → Tauri 命令
// 注册 → 重投函数 → 面板文案。最典型的失效方式是「界面还显示重新同步，但命令
// 没注册」，点下去只报错；或反过来，能力已在后端可用，界面却不给入口。
// 这里把每层的必备件都锁住，并额外锁住「原因文本必须先清洗」——last_error 来自
// 后端日志，带换行和超长 JSON 片段时会直接把面板撑成多行、挤掉右侧指标。
import { strict as assert } from 'node:assert';
import { readFileSync } from 'node:fs';

const read = (relative: string) => readFileSync(new URL(relative, import.meta.url), 'utf8');

const dashboard = read('../src/pages/DashboardPage.tsx');
const main = read('../src/main.tsx');
const api = read('../src/services/agentApi.ts');
const styles = read('../styles.css').replace(/\/\*[\s\S]*?\*\//g, '');
const commands = read('../../src/app/commands.rs');
const ui = read('../../src/app/ui.rs');

// 1. 面板只在存在死信时给入口，并且必须先把失败原因清洗再展示。
assert.match(dashboard, /function ProjectionStatusPanel\(/, 'DashboardPage 缺少同步状态面板');
assert.match(dashboard, /status\.dead_letter_reasons\?\.\[0\]/, '面板要取最主要的一类失败原因，逐条罗列看不清主要矛盾');
assert.match(dashboard, /function summarizeProjectionReason\(raw: string\)/, '失败原因需要先折叠空白并截断再展示');
assert.match(dashboard, /summarizeProjectionReason\(primaryReason\.last_error\)/,
  '失败原因必须经过清洗函数，不能把 last_error 原样插进描述');
assert.match(dashboard, /条任务同步失败，本地运行不受影响/,
  '死信描述要让用户确认本地运行未受影响，否则会被误读成任务失败');
assert.match(dashboard, /\{deadLetter \? \(/, '重新同步按钮只能在有死信时出现');
assert.match(dashboard, /onClick=\{onRequeue\} disabled=\{requeueBusy\}/, '重新同步按钮要在请求进行中禁用，避免重复重投');
assert.match(dashboard, /重新同步\s*<\/button>/, '重新同步按钮文案缺失');
assert.match(dashboard, /className="projection-sync-metrics"[\s\S]{0,400}?同步失败/, '面板要给出同步失败计数');

// 2. 主窗口把命令接起来，并在重投后刷新状态、按剩余死信给出不同反馈。
assert.match(main, /agentApi\.requeueProjectionDeadLetters\(\)/, '主窗口必须调用重投命令');
assert.match(main, /await refreshProjectionSyncStatus\(\);/, '重投后要刷新同步状态，否则面板停留在旧数字');
assert.match(main, /dead_letter_after === 0/, '要区分「全部恢复」和「仍有剩余」两种结果');
assert.match(main, /projectionRequeueBusy=\{projectionRequeueBusy\}/, 'DashboardPage 的忙碌态要透传下去');
assert.match(main, /onRequeueProjectionDeadLetters=\{requeueProjectionDeadLetters\}/, 'DashboardPage 的重投入口要接线');

// 3. 前端调用的命令名必须真的在后端注册，且实现落到同一个重投函数上。
assert.match(api, /'requeue_projection_dead_letters'/, 'agentApi 的命令名要与后端一致');
assert.match(api, /ProjectionRequeueReport/, '前端要按报告类型读取重投结果');
assert.match(commands, /pub\(crate\) async fn requeue_projection_dead_letters\(/, '后端缺少重投命令');
assert.match(commands, /agent_core_projection::requeue_dead_letter_projections\(fragment\)/,
  '命令必须复用 agent_core_projection 的重投实现，不能另写一套');
assert.match(ui, /requeue_projection_dead_letters/, '命令未注册到 Tauri invoke_handler 就点不动');

// 4. 面板是「文字 + 指标 + 操作」三段式：按钮没被定位时会换行到第二行，把卡片顶成两层。
assert.match(styles, /\.projection-sync-panel\s*\{[^}]*grid-template-columns: minmax\(0, 1fr\) auto auto/,
  '面板要用三段网格容纳文字、指标与操作');
assert.match(styles, /\.projection-sync-panel > \.btn\s*\{[^}]*grid-column: 3/,
  '重新同步按钮要固定在第三列，否则会掉到下一行');
assert.match(styles, /\.projection-sync-panel\.error\s*\{[^}]*background/,
  '死信态要有独立的错误配色，不能与已同步共用中性样式');

console.log('projection-requeue UI checks passed');
