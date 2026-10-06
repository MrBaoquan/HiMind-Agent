// 引擎能力的落点回归自检（零依赖：node --experimental-strip-types）。
//
// 结论：引擎构建不占一级菜单，它以工作流（+ Agent 能力）交付；
// Agent 界面只负责「用哪个 Unity / Unreal 编辑器」这类本机配置。
// 这里把这两条锁住，防止又长回一个「工程」页面。
import { strict as assert } from 'node:assert';
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const readFrontend = (relative: string) => readFileSync(join(here, '..', relative), 'utf8');
const readAgent = (relative: string) => readFileSync(join(here, '..', '..', relative), 'utf8');
const agentPath = (relative: string) => join(here, '..', '..', relative);

const navigation = readFrontend(join('src', 'navigation.ts'));
const types = readFrontend(join('src', 'types.ts'));
const main = readFrontend(join('src', 'main.tsx'));
const settings = readFrontend(join('src', 'pages', 'SettingsPage.tsx'));
const api = readFrontend(join('src', 'services', 'agentApi.ts'));
const serviceRs = readAgent(join('src', 'capability', 'service.rs'));

// 1. 不允许再出现「工程」一级菜单或独立页面。
assert.ok(!/key: 'engine'/.test(navigation), '导航里不能再有 engine 一级菜单');
assert.ok(!/label: '工程'/.test(navigation), '导航里不能再有「工程」分组');
assert.ok(!/'engine'/.test(types), 'PageKey 不应再包含 engine');
assert.ok(!/EngineBuildPage/.test(main), 'main.tsx 不应再有工程构建页面分支');
assert.ok(!existsSync(agentPath(join('frontend', 'src', 'pages', 'EngineBuildPage.tsx'))), '独立的工程构建页面应已删除');

// 2. 构建以工作流交付：包结构、能力步骤、阻塞等待、声明式表单。
const workflow = JSON.parse(readAgent(join('workflows', 'engine-build', 'workflow.json')));
assert.equal(workflow.schema_version, 'workflow_package.v1');
assert.equal(workflow.id, 'com.himind.workflow.engine-build');
assert.ok(workflow.capabilities.includes('exhibit.workspace.build'), '工作流必须声明构建能力');
const buildStep = workflow.steps.find((step: { id: string }) => step.id === 'ENGINE-BUILD');
assert.ok(buildStep, '工作流缺少构建步骤');
assert.equal(buildStep.capability_id, 'exhibit.workspace.build');
assert.equal(buildStep.execution_mode, 'long_running', '构建要按长任务声明');
assert.equal(buildStep.input.wait, true, '构建步骤必须要求等待终态，否则运行会秒完成');
assert.equal(workflow.ui.mode, 'declarative');
assert.ok(existsSync(agentPath(join('workflows', 'engine-build', 'ui', 'workflow-view.json'))), '工作流缺少声明式表单');

// 3. 表单字段必须落在能力入参里，否则会被 strict schema 过滤掉。
const view = JSON.parse(readAgent(join('workflows', 'engine-build', 'ui', 'workflow-view.json')));
const fieldIds = view.sections.flatMap((section: { fields: Array<{ id: string }> }) => section.fields.map((field: { id: string }) => field.id));
for (const field of ['target_path', 'engine_type', 'engine_version', 'provider', 'target_platform', 'architecture', 'configuration', 'output_path', 'clean']) {
  assert.ok(fieldIds.includes(field), `表单缺少能力入参：${field}`);
}
const buildSchema = serviceRs.slice(
  serviceRs.indexOf('"exhibit.workspace.build"'),
  serviceRs.indexOf('"exhibit.workspace.build.status"'),
);
for (const field of fieldIds) {
  assert.ok(new RegExp(`"${field}"`).test(buildSchema), `能力 schema 不接受表单字段：${field}`);
}
assert.ok(/"wait"/.test(buildSchema), '能力 schema 缺少 wait，工作流塞不进去');

// 4. Agent 界面只保留引擎编辑器配置：Unity 与 Unreal 都能配，并能选本机已装版本。
assert.ok(/engineInstallations\(\)/.test(settings), '设置页要读取本机引擎清单');
assert.ok(/saveEngineEditor\('unreal'/.test(settings) && /saveEngineEditor\('unity'/.test(settings), '设置页要能保存两种编辑器路径');
assert.ok(/engine-installation/.test(settings), '设置页要能一键选用已发现的引擎版本');
assert.ok(/unreal_editor_path/.test(api), '编辑器设置类型缺少 Unreal 字段');

console.log('engine UI checks passed');
