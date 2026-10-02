// 「运行环境」命名的回归自检（零依赖：node --experimental-strip-types）。
//
// 运行环境同时出现在工作流详情、AI 连接页和设置页。这三个地方以前各自写了一遍
// provider → 名字的判断，最容易出的两个错是：
//   1. 用 includes('copilot') 认品牌，把用户接入的 acp.github-copilot 认成本机 CLI；
//   2. 工作流详情只拿到 provider id，不知道用户给客户端起的名字。
// 这里把口径锁死，后面加执行后端时只需要改 runtimeProviderView 一处。
import { strict as assert } from 'node:assert';
import { readFileSync } from 'node:fs';
import { acpPresets } from '../src/pages/acpProfileView.ts';
import { LOCAL_RUNTIME_META, isAcpProvider, localRuntimeMeta, localRuntimeStatus, runtimeProviderLabel } from '../src/pages/runtimeProviderView.ts';

/** 读页面源码，用来断言「名字只有一个来源」这件事没有被绕过。 */
function source(file: string): string {
  return readFileSync(new URL(`../src/pages/${file}`, import.meta.url), 'utf8');
}

const codexProfile = {
  provider_id: 'acp.codex',
  display_name: '我的 Codex',
  executable: 'npx',
  args: ['-y', '@agentclientprotocol/codex-acp@1.12.0'],
  version: '1.12.0',
  permission_policy: 'prompt',
  enabled: true,
};

// acp.* 永远先按「用户接入的运行环境」解释，不能被品牌子串截胡。
assert.equal(runtimeProviderLabel('acp.codex', [codexProfile]), '我的 Codex');
assert.equal(runtimeProviderLabel('acp.github-copilot', [{ ...codexProfile, provider_id: 'acp.github-copilot', display_name: '公司 Copilot' }]), '公司 Copilot');
// 没有登记信息时（历史运行记录里的客户端已被删除）落到内置预设名，不能只剩一串 ID。
assert.equal(runtimeProviderLabel('acp.github-copilot'), 'GitHub Copilot');
assert.equal(runtimeProviderLabel('acp.opencode'), 'OpenCode');
// 预设之外的客户端没有名字可用，退到原样显示 ID，不能凭空编一个名字。
assert.equal(runtimeProviderLabel('acp.my-codex'), 'acp.my-codex');

// 未指定执行方是「自动选择」，不是空字符串。
assert.equal(runtimeProviderLabel(), '自动选择');
assert.equal(runtimeProviderLabel(''), '自动选择');

// 本机自带后端的名字必须和探测口径一致。
assert.equal(runtimeProviderLabel('himind.builtin'), 'HiMind AI');
assert.equal(runtimeProviderLabel('personal.codex'), 'Codex');
assert.equal(runtimeProviderLabel('personal.github-copilot'), 'GitHub Copilot');
// 品牌兜底：历史记录里的 deepseek 变体也算 HiMind AI。
assert.equal(runtimeProviderLabel('himind.deepseek-harness'), 'HiMind AI');
// 认不出来的后端，文案要能提示「这不是内置的」，而不是显示空。
assert.equal(runtimeProviderLabel('someone.else'), '自定义运行环境');
assert.ok(runtimeProviderLabel('someone.else').trim());

// 表里必须有这三家：AI 连接页的「本机已安装」列表按 provider id 取名。
assert.deepEqual(
  Object.keys(localRuntimeMeta('himind.builtin')).sort(),
  ['detail', 'icon', 'name'],
);
assert.equal(localRuntimeMeta('personal.codex').icon, 'code');
assert.equal(localRuntimeMeta('personal.github-copilot').icon, 'github');
// detail 不能写成「本机安装的 X」：未安装时整行会变成「本机安装的 X · 未安装」。
for (const provider of Object.keys(LOCAL_RUNTIME_META)) {
  assert.ok(!LOCAL_RUNTIME_META[provider].detail.includes('安装'), `${provider} 的 detail 不能声称已安装`);
}
// 未知后端不能显示成空白行。
const unknown = localRuntimeMeta('someone.else');
assert.equal(unknown.name, 'someone.else');
assert.ok(unknown.detail.trim());

// acp 判定要跟后端 runtime::acp::is_provider 同口径。
assert.ok(isAcpProvider('acp.stdio'));
assert.ok(isAcpProvider('acp.codex'));
assert.ok(!isAcpProvider('personal.codex'));
assert.ok(!isAcpProvider('himind.builtin'));

// 状态文案区分「没装」和「装了但用不了」：后者是红色，用户才会去排查。
// 「已就绪」是刻意的：和已接入列表、概览统计用同一个词。
assert.deepEqual(localRuntimeStatus('ready'), { label: '已就绪', kind: 'success' });
assert.deepEqual(localRuntimeStatus('incompatible'), { label: '需要更新', kind: 'warn' });
assert.deepEqual(localRuntimeStatus('unsupported'), { label: '不可用', kind: 'danger' });
assert.deepEqual(localRuntimeStatus('unavailable'), { label: '未安装', kind: 'neutral' });
for (const status of ['ready', 'incompatible', 'unsupported', 'unavailable', '', 'nonsense']) {
  assert.ok(localRuntimeStatus(status).label.trim(), `状态 ${status} 必须有可读文案`);
}

// 内置预设的名字要和这里的兜底一致，否则「已删除的客户端」和「未接入的预设」会显示两个名字。
for (const preset of acpPresets) {
  assert.equal(runtimeProviderLabel(`acp.${preset.providerId}`), preset.name);
}

// 光有正确的函数不够：页面各自再写一遍品牌名，口径还是会分叉。
// 下面这几条锁的是接线——谁渲染「运行环境」，名字就必须来自 runtimeProviderView。
const panel = source('AcpProfilesPanel.tsx');
const workflows = source('WorkflowsPage.tsx');
const settings = source('SettingsPage.tsx');

assert.ok(
  /from '\.\/runtimeProviderView'/.test(panel),
  'AI 连接页的本机列表要从 runtimeProviderView 取名，不能自己写品牌表',
);
// 面板里出现 'HiMind AI' 这类字面量，说明又开始在页面里攒第二份品牌表了。
for (const brand of ['HiMind AI', 'GitHub Copilot']) {
  assert.ok(
    !panel.includes(`'${brand}'`) && !panel.includes(`>${brand}<`),
    `AcpProfilesPanel 不能写死品牌名 ${brand}：名字由 localRuntimeMeta 决定`,
  );
}
// 本机清单必须排除 acp.*：它们是「已接入」，跟着登记表显示，不是探测出来的本机后端。
assert.ok(/!isAcpProvider\(/.test(panel), '本机已安装列表要用 isAcpProvider 排除 acp.*');
// 三段标题是「已装 / 可接 / 已接」这套心智的骨架，改名要连着改这里的断言。
for (const heading of ['本机已安装', '可接入的运行环境', '已接入的运行环境']) {
  assert.ok(panel.includes(heading), `AI 连接页缺少分区标题「${heading}」`);
}

assert.ok(
  /import \{ runtimeProviderLabel \} from '\.\/runtimeProviderView'/.test(workflows),
  '工作流详情要从 runtimeProviderView 取运行环境名',
);
assert.ok(
  !/function runtimeProviderLabel/.test(workflows) && !/function localRuntimeMeta/.test(workflows),
  '工作流页不能自带一份运行环境命名函数',
);
assert.ok(
  /runtimeProviderLabel\(runDetail\.run\.runtime_provider, runtimeProfiles\)/.test(workflows),
  '工作流详情的「运行环境」要带上已接入客户端名，否则 acp.* 只能显示 ID',
);

for (const provider of ['himind.builtin', 'personal.codex', 'personal.github-copilot']) {
  assert.ok(
    settings.includes(`localRuntimeMeta('${provider}')`),
    `设置页的执行工具下拉要取 localRuntimeMeta('${provider}') 的名字`,
  );
}

console.log('runtime provider checks passed（运行环境命名 / 品牌判定 / 探测状态 / 页面接线）');
