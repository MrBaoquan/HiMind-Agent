// Mission deep-link validation and entry wiring regression test.
import { strict as assert } from 'node:assert';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { MISSION_OPEN_EVENT, initialMissionOpen, missionIdFromBootstrap, validMissionId } from '../src/pages/missionDeepLink.ts';

assert.equal(validMissionId('mission_abc-123'), 'mission_abc-123');
assert.equal(validMissionId('  mission_abc  '), 'mission_abc');
assert.equal(validMissionId(''), '');
assert.equal(validMissionId('   '), '');
assert.equal(validMissionId('a'.repeat(160)), 'a'.repeat(160));
assert.equal(validMissionId('a'.repeat(161)), '');
for (const invalid of ['bad/id', 'mission abc', 'mission.abc', '../mission', 'mission\nabc', '任务', null, undefined, 42, {}]) {
  assert.equal(validMissionId(invalid), '', `accepted invalid Mission ID: ${String(invalid)}`);
}
assert.equal(missionIdFromBootstrap({ mission_id: 'mission_bootstrap' }), 'mission_bootstrap');
assert.equal(missionIdFromBootstrap({ mission_id: 'bad/id' }), '');
assert.equal(missionIdFromBootstrap(null), '');
assert.equal(initialMissionOpen({ mission_id: 'mission_first' }), 'mission_first');
assert.equal(MISSION_OPEN_EVENT, 'himind:mission-navigate');

const here = dirname(fileURLToPath(import.meta.url));
const main = readFileSync(join(here, '..', 'src', 'main.tsx'), 'utf8');
const taskCenter = readFileSync(join(here, '..', 'src', 'pages', 'TaskCenterPage.tsx'), 'utf8');
const rustMain = readFileSync(join(here, '..', '..', 'src', 'main.rs'), 'utf8');
const rustUi = readFileSync(join(here, '..', '..', 'src', 'app', 'ui.rs'), 'utf8');

assert.ok(/initialMissionOpen\(\)/.test(main), '首屏必须读取 Mission bootstrap');
assert.ok(/if \(initialMissionOpen\(\)\) return 'tasks'/.test(main), '有效 Mission 深链首屏必须落到我的任务');
assert.ok(/listen<MissionOpenBootstrap>\(MISSION_OPEN_EVENT/.test(main), '运行中的主窗口必须监听 Mission 导航事件');
assert.ok(/setPage\('tasks'\)/.test(main.slice(main.indexOf('listen<MissionOpenBootstrap>'), main.indexOf('listen<MissionOpenBootstrap>') + 1200)), 'Mission 事件必须切到我的任务');
assert.ok(/missionNavigationRevision=\{missionTarget\.revision\}/.test(main), '同一 Mission 重复导航必须用 revision 触发');
assert.ok(/initialMissionId=\{missionTarget\.missionId\}/.test(main), 'TaskCenterPage 必须收到 Mission 选择');
assert.ok(/useState\(initialMissionId \|\| ''\)/.test(taskCenter), 'TaskCenterPage 必须用深链目标初始化详情');
assert.ok(/__HIMIND_MISSION_OPEN__/.test(rustUi), '主窗口必须注入 Mission bootstrap');
assert.ok(/himind:mission-navigate/.test(rustUi), '原生层必须发送 Mission 导航事件');
assert.ok(/open == "mission"/.test(rustMain) && /mission_abc-123/.test(rustMain), '协议解析必须接受 Mission 深链并有回归测试');
console.log('mission deep-link checks passed');
