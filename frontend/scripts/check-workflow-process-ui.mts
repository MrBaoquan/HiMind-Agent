import { strict as assert } from 'node:assert';
import { readFileSync } from 'node:fs';

const page = readFileSync(new URL('../src/pages/WorkflowsPage.tsx', import.meta.url), 'utf8');
const rail = readFileSync(new URL('../src/components/WorkflowProcessRail.tsx', import.meta.url), 'utf8');
const styles = readFileSync(new URL('../styles.css', import.meta.url), 'utf8');

const count = (value: string, needle: string) => value.split(needle).length - 1;

// 定义页只允许一个过程轨道；旧的“执行结构 + 步骤摘要”双列表不能回退。
assert.equal(count(page, '<WorkflowProcessRail nodes={selectedWorkflowGraph?.nodes || []} mode="definition" />'), 1, '定义页必须有且只有一个过程轨道');
assert.equal(count(page, '<WorkflowProcessRail nodes={timeline?.steps || []} mode="run" activities={activity} />'), 1, '运行页必须有且只有一个过程轨道');
assert.equal(count(page, '<strong>执行结构</strong>'), 0, '执行结构不能与过程轨道重复');
assert.equal(count(page, '<strong>步骤摘要</strong>'), 0, '步骤摘要不能与过程轨道重复');
assert.equal(count(page, '<strong>任务动态</strong>'), 0, '任务动态不能再作为与步骤并列的主区块');
assert.equal(count(page, 'workflow-run-step-timeline'), 0, '旧步骤列表不能回退');
assert.equal(count(page, 'workflow-run-activity'), 0, '旧事件主列表不能回退');
assert.equal(count(page, 'workflow-run-progress'), 0, '平滑百分比进度条不能回退');

// 运行上下文只保留环境、模型、工作区；当前步骤和耗时各只有一个主展示位置。
assert.equal(page.includes('<span>当前步骤</span>'), false, '运行上下文不能再重复当前步骤');
assert.equal(page.includes('<span>耗时</span>'), false, '运行上下文不能再重复耗时');
assert.equal(page.includes('<span>工作流</span><strong>'), false, '运行上下文不能再重复工作流名称');
assert.match(page, /<span>运行环境<\/span>/, '运行上下文必须保留运行环境');
assert.match(page, /<span>AI 模型<\/span>/, '运行上下文必须保留 AI 模型');
assert.match(page, /<span>工作区<\/span>/, '运行上下文必须保留工作区');

// 六类状态均要有一句非颜色依赖的反馈。
for (const text of ['已排队，等待可用运行环境', '运行中', '等待处理，完成后会继续运行', '全部步骤已完成', '运行失败，后续步骤已停止', '运行已取消']) {
  assert.ok(page.includes(text), `缺少运行状态反馈：${text}`);
}

// 状态必须由同一图标位形变，且只有活动节点持续转动。
assert.match(rail, /<MorphIcon icon=/, '过程轨道必须使用 MorphIcon');
assert.equal(count(rail, 'workflow-process-node-state.is-spinning svg'), 0, '组件内不能用选择器字符串表达状态');
assert.equal(count(rail, "runStep?.state === 'active' ? ' is-spinning' : ''"), 1, '只有活动节点允许持续转动');
assert.match(styles, /\.workflow-process-node-state\.is-spinning svg \{ animation: spin/, '活动节点需要唯一的持续转动样式');

// 默认只给当前、等待或失败步骤展示最近事件；完整事件留在节点内折叠。
assert.match(rail, /showLatestActivity = Boolean\(runStep && \['active', 'waiting', 'failed'\]\.includes\(runStep\.state\)\)/, '最近事件只应服务当前/等待/失败步骤');
assert.match(rail, /<details className="workflow-process-events"/, '每个步骤必须有可展开事件详情');
assert.match(styles, /\.workflow-process-events \{/, '过程事件折叠样式必须存在');
assert.match(styles, /@media \(max-width: 760px\)[\s\S]*\.workflow-process-node-card \{ grid-template-columns: 20px minmax\(0, 1fr\); \}/, '窄窗口过程节点必须收回为单列内容');

console.log('workflow process UI checks passed');
