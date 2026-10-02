// 安装 / 发布计划面的回归自检（零依赖：node --experimental-strip-types）。
//
// 「会发生什么」这件事在三个地方重复出现（技能安装、插件安装、拓展发布），
// 任何一处各写一套判断都会重新分叉：按钮说能装、后端说有阻断；计划里说没有
// 依赖、发布时才发现没锁版本。这里锁三件事：
//   1. 依赖动作的取值全部登记过中文标签 —— 漏登记就会把 `resolve`、
//      `unavailable` 这类英文枚举直接端到用户面前；
//   2. 计划卡的落点行把路径放在独占的一行（路径是最长、最不能截断的那一列）；
//   3. 三处调用点都渲染同一张计划卡，并且按钮的可点状态由后端 `ready` 决定，
//      不由界面自己猜。
import { strict as assert } from 'node:assert';
import { readFileSync } from 'node:fs';

import {
  DEPENDENCY_ACTIONS,
  SCOPE_KEYS,
  STRATEGY_KEYS,
  dependencyActionKnown,
  dependencyActionLabel,
  dependencyActionTone,
  dependencyNeedsWork,
  scopeLabel,
  strategyLabel,
} from '../src/components/operationPlanText.ts';

const read = (relative: string) => readFileSync(new URL(relative, import.meta.url), 'utf8');
const card = read('../src/components/OperationPlanCard.tsx');
const styles = read('../styles.css').replace(/\/\*[\s\S]*?\*\//g, '');
const skillsPage = read('../src/pages/SkillsWorkspacePage.tsx');
const pluginsPage = read('../src/pages/PluginsPage.tsx');
const developmentPage = read('../src/pages/ExtensionDevelopmentPage.tsx');
const main = read('../src/main.tsx');
const agentApi = read('../src/services/agentApi.ts');
const pluginManager = read('../../src/app/plugin_manager.rs');
const skillManager = read('../../src/app/skill_manager.rs');
const operationPlan = read('../../src/app/operation_plan.rs');

const styleBlocks = [...styles.matchAll(/([^{}]+)\{([^{}]*)\}/g)].map((match) => ({
  selector: match[1].trim(),
  body: match[2].replace(/\s+/g, ' ').trim(),
}));
const lastBlock = (selector: string) =>
  [...styleBlocks].reverse().find((block) =>
    block.selector.split(',').map((part) => part.trim()).includes(selector));

// 1. 依赖动作：每个取值都要有中文标签，并且状态判定跟着取值走。
for (const action of DEPENDENCY_ACTIONS) {
  assert.ok(dependencyActionKnown(action), `${action} 未被登记`);
  assert.match(dependencyActionLabel(action), /[\u4e00-\u9fa5]/,
    `依赖动作 ${action} 的中文标签不能是原文：用户看到的是英文枚举`);
}
assert.equal(dependencyActionLabel('install'), '将安装');
assert.equal(dependencyActionLabel('satisfied'), '已就绪');
assert.equal(dependencyActionLabel('keep'), '已就绪');
// 发布计划里"只有最低版本、没锁 pin"这一档，以前会把 `resolve` 直接显示出来。
assert.equal(dependencyActionLabel('resolve'), '未锁定版本');
assert.equal(dependencyActionTone('satisfied'), 'ready');
assert.equal(dependencyActionTone('keep'), 'ready');
assert.equal(dependencyActionTone('install'), 'pending');
assert.equal(dependencyActionTone('blocked'), 'blocked');
assert.equal(dependencyActionTone('unavailable'), 'blocked');
// 已满足的依赖不算"需要处理"，否则计数永远是全部依赖。
assert.equal(dependencyNeedsWork('satisfied'), false);
assert.equal(dependencyNeedsWork('keep'), false);
assert.equal(dependencyNeedsWork('update'), true);
assert.equal(dependencyNeedsWork('resolve'), true);
// 未登记的取值退回原值：宁可显示原文，也不要显示空白。
assert.equal(dependencyActionLabel('brand-new'), 'brand-new');
assert.equal(dependencyActionKnown('brand-new'), false);

// 2. 取值集合必须覆盖后端真正会写出来的动作词。后端新增一档而界面没登记时，
//    这里会失败——这正是"英文枚举漏到界面上"的唯一入口。
const actionLiterals = new Set<string>();
for (const source of [pluginManager, skillManager, operationPlan]) {
  for (const match of source.matchAll(/^\s*action:\s*(.*)$/gm)) {
    for (const literal of match[1].matchAll(/"([a-z_]+)"/g)) actionLiterals.add(literal[1]);
  }
  for (const match of source.matchAll(/\.action\s*=\s*"([a-z_]+)"/g)) actionLiterals.add(match[1]);
  // 依赖解析器用 `let (action, reason) = ...` 的元组给出结论（如
  // `("satisfied", "本机版本已满足")`），它同样是一个动作取值。
  for (const match of source.matchAll(/\(\s*"([a-z_]+)"\s*,\s*"[^"]*[\u4e00-\u9fa5]/g)) {
    actionLiterals.add(match[1]);
  }
}
assert.ok(actionLiterals.size >= 5, `没有从后端读到依赖动作词：${[...actionLiterals].join(' / ')}`);
for (const action of actionLiterals) {
  assert.ok(dependencyActionKnown(action),
    `后端会写出依赖动作 ${action}，界面没有对应标签（会原样显示英文）`);
}

// 落点标签同理：scope / strategy 也来自后端枚举。
for (const scope of ['agent', 'user', 'project', 'organization', 'remote']) {
  assert.ok(SCOPE_KEYS.includes(scope), `落点作用域 ${scope} 没有标签`);
  assert.match(scopeLabel(scope), /[\u4e00-\u9fa5]/, `落点作用域 ${scope} 的中文标签不能是原文`);
}
for (const strategy of ['store', 'copy', 'symlink', 'extract', 'release', 'submit']) {
  assert.ok(STRATEGY_KEYS.includes(strategy), `落盘策略 ${strategy} 没有标签`);
  assert.match(strategyLabel(strategy), /[\u4e00-\u9fa5]/, `落盘策略 ${strategy} 的中文标签不能是原文`);
}

// 3. 计划卡本身：阻断原因、落点、依赖、步骤各一段，且由开关控制显隐。
assert.match(card, /plan\.blocked_reasons\.map\(reason => <div className="plan-line danger"/,
  '计划卡必须先说明"为什么现在不能做"，且用危险色');
assert.match(card, /plan\.warnings\.map\(warning => <div className="plan-line warn"/, '告警要有独立的表达');
assert.match(card, /const targets = showTargets \? plan\.targets : \[\]/, '落点区块要受 showTargets 控制');
// 计数不能脱离列表：技能弹窗隐藏落点列表时若还写「0 个落点」，等于自己打自己的脸。
assert.match(card, /\{showTargets \? <small>\{created \?/, '落点计数必须跟落点列表同时出现');
assert.match(card, /const dependencies = showDependencies \? plan\.dependencies : \[\]/,
  '依赖区块要受 showDependencies 控制');
assert.match(card, /const steps = showSteps \? plan\.steps : \[\]/, '步骤区块要受 showSteps 控制');
assert.match(card, /dependencyNeedsWork\(dependency\.action\)/, '依赖计数要按状态算，不能按取值名字面比较');
assert.match(card, /dependencyActionTone\(dependency\.action\)/, '依赖行的状态不能各处自己判断');
// 标签只有一份：卡里再抄一张映射表，两处就会漂移。
assert.doesNotMatch(card, /SCOPE_LABELS|STRATEGY_LABELS/, '标签表只能有一份，卡里不能再内联');

const targetRow = lastBlock('.plan-target-row');
assert.ok(targetRow, 'styles.css 缺少 .plan-target-row 规则');
assert.equal((targetRow!.body.match(/minmax\(0, 1fr\)/) || []).length, 1,
  '落点行第一排只放名称与标签，路径要另起一行');
const targetPath = lastBlock('.plan-target-path');
assert.ok(targetPath, 'styles.css 缺少 .plan-target-path 规则');
const pathBlocks = styleBlocks.filter((block) =>
  block.selector.split(',').map((part) => part.trim()).includes('.plan-target-path'));
assert.ok(pathBlocks.some((block) => /grid-column: 1 \/ -1/.test(block.body)),
  '落点路径要独占整行：和标签挤在同一排时，路径总是被压到只剩几个字符');
assert.doesNotMatch(targetPath!.body, /text-overflow: ellipsis/, '路径不能用省略号收尾');
const targetTag = lastBlock('.plan-tag');
assert.ok(targetTag && /white-space: nowrap/.test(targetTag.body), 'scope / 策略标签不能被折成两行');
assert.ok(lastBlock('.plan-dependency-row.blocked'), '被阻断的依赖要有独立底色');

// 4. 三处调用点共用同一张卡，并且按钮跟着后端的 ready 走。
assert.match(skillsPage, /plan\.plan \? <OperationPlanCard plan=\{plan\.plan\} heading="这次会做什么" showTargets=\{false\} showDependencies=\{false\} \/>/,
  '技能安装弹窗要渲染计划卡（落点与依赖由下方可勾选清单承担，避免写两遍）');
assert.match(skillsPage, /disabled=\{!plan\?\.ready \|\| busy\}/,
  '技能安装按钮要由后端的 ready 决定，点了才知道被拒是最差的一种');
assert.match(skillsPage, /const targetPaths = new Map\(\(plan\?\.plan\?\.targets \|\| \[\]\)/,
  '投放目标清单要用计划里的真实落点做悬停说明');
assert.match(pluginsPage, /plan\.plan \? <OperationPlanCard plan=\{plan\.plan\} heading="这次会做什么" showDependencies=\{false\} \/>/,
  '插件安装弹窗要渲染计划卡（依赖由下方逐项勾选行承担）');
assert.match(pluginsPage, /disabled=\{!plan\?\.ready\}/, '插件安装按钮要由后端的 ready 决定');
assert.match(developmentPage, /plan \? <OperationPlanCard plan=\{plan\} heading="发布计划" \/> : null/,
  '拓展发布要显示发布计划');
assert.match(developmentPage, /const plan = preview\?\.plan;/, '发布计划取自发布预览');
assert.match(developmentPage, /\(plan\?\.ready \?\? true\)/,
  '发布按钮要读发布计划的 ready；计划缺失时才退回原来的判定');

// 5. 客户端能力矩阵（P0-2）必须在界面里真的被用上，而不是只有后端命令。
assert.match(agentApi, /clientCapabilityMatrix: \(\) => invoke<ClientCapabilityMatrix>\('get_client_capability_matrix'\)/,
  '前端要能读到客户端能力矩阵');
assert.match(main, /agentApi\.clientCapabilityMatrix\(\)/, '主界面启动时要拉取能力矩阵');
assert.match(main, /skillClientDescriptors\(skillStatus, mcpTargets, clientMatrix\)/,
  '客户端清单要以矩阵为准，不能再各自推断支持级别');

console.log(`operation plan checks passed（${DEPENDENCY_ACTIONS.length} 个依赖动作 / ${actionLiterals.size} 个后端取值）`);
