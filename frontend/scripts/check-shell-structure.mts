// 应用外壳结构的回归自检（零依赖：node --experimental-strip-types）。
// 这里锁两类「只在真机上才看得出来」的缺陷：
// 1. 菜单栏被渲染两次 —— 窗口上会直接出现两条标题栏。
// 2. 行内徽标被容器规则改成上下堆叠 —— 「已启用」这类胶囊会竖排换行。
import { strict as assert } from 'node:assert';
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const shell = readFileSync(join(here, '..', 'src', 'components', 'Shell.tsx'), 'utf8');
const styles = readFileSync(join(here, '..', 'styles.css'), 'utf8');
const workflowsPage = readFileSync(join(here, '..', 'src', 'pages', 'WorkflowsPage.tsx'), 'utf8');
const pluginsPage = readFileSync(join(here, '..', 'src', 'pages', 'PluginsPage.tsx'), 'utf8');
const schedulesPage = readFileSync(join(here, '..', 'src', 'pages', 'SchedulesPage.tsx'), 'utf8');

function countOccurrences(haystack: string, needle: string): number {
  return haystack.split(needle).length - 1;
}

// 菜单栏只能有一个挂载点。定义处写的是 `function AppMenuBar(`，因此这里只数 JSX 用法。
assert.equal(countOccurrences(shell, '<AppMenuBar'), 1, 'Shell 只能渲染一个 AppMenuBar，多了就是双重标题栏');
assert.equal(countOccurrences(shell, 'className="app-menu-bar"'), 1, 'app-menu-bar 只能出现一次');

// 自绘标题栏必须阻止 WebView2 的默认鼠标手势，否则 startDragging 会晚于
// 浏览器焦点/选择动作，用户按住空白处时窗口不动。拖拽区也必须显式标注。
const dragHandler = shell.match(/const handleTitleBarMouseDown[\s\S]*?\n    \};/);
assert.ok(dragHandler, '标题栏必须有 mousedown 拖拽处理器');
assert.match(dragHandler[0], /event\.preventDefault\(\)/, '标题栏拖拽处理必须先 preventDefault');
assert.match(dragHandler[0], /getCurrentWindow\(\)\.startDragging\(\)/, '标题栏拖拽必须使用 Tauri Window API');
assert.match(shell, /className="app-menu-brand"[^>]*data-tauri-drag-region/, '标题栏品牌区必须声明 drag region');
assert.match(shell, /className="app-menu-drag-space"[^>]*data-tauri-drag-region/, '标题栏空白区必须声明 drag region');

// 徽标安全网必须保留：列表里 `button > span { display: grid }` 这类规则会把
// 「圆点 + 文字」的胶囊拆成两行，必须由后面的 !important 规则兜住。
const badgeSafetyNet = styles.match(/:where\([^{]*\.pill[^{]*\{[^}]*display: inline-flex !important;[^}]*\}/);
assert.ok(badgeSafetyNet, 'styles.css 必须保留徽标 inline-flex 安全网');
assert.ok(/min-width: max-content !important/.test(badgeSafetyNet[0]), '徽标不能被压到换行，必须 min-width: max-content');

// 会被容器规则波及的横向展开列表：徽标所在的网格列必须是 auto，不能被压成 0 宽。
const badgeColumn = 'grid-template-columns: 30px minmax(0, 1fr) auto;';
assert.ok(new RegExp(`\\.workflow-list > button \\{[^}]*${badgeColumn.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}`).test(styles), '工作流列表的徽标列要保持 auto');
assert.ok(new RegExp(`\\.schedule-list-item > button:first-child \\{[^}]*${badgeColumn.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}`).test(styles), '定时计划列表的徽标列要保持 auto');

// 执行结构只保留一条统一过程轨道；图摘要和步骤摘要都不能再另起一套列表。
assert.ok(/<WorkflowProcessRail nodes=\{selectedWorkflowGraph\?\.nodes \|\| \[\]\} mode="definition"/.test(workflowsPage), '执行结构必须走统一过程轨道');
assert.ok(!/workflow-graph-summary/.test(workflowsPage), '定义页不能再回到重复的图摘要');

// 默认启用是常态：插件行不能再为每一行多渲染一行「已启用」。
assert.ok(!/已启用' : '已停用/.test(pluginsPage), '插件列表行不能重复渲染默认的「已启用」文案');
assert.ok(/pluginListStateText\(item\)/.test(pluginsPage), '插件列表行只对需要留意的状态补文字');

// 运行态必须能持续变化：计划行在跑时要给出走动的「已运行」。
assert.ok(/已运行 \$\{formatElapsedCn/.test(schedulesPage), '定时计划列表在运行中要显示走动的已运行时长');
assert.ok(/liveRuns\.get\([^)]*\)\?\.label/.test(schedulesPage), '计划详情要读 liveRuns 的 label 字段');

// 运行号只能走 shortRunId 收敛：直接 slice(-8) 会把 `...:1790228443195_6480`
// 截成半截 `674_6480`，同一串在列表、详情、任务中心里长得不一样。
const runIdSlicing = [workflowsPage, schedulesPage].filter((source) => /\.run_id\.slice\(/.test(source));
assert.equal(runIdSlicing.length, 0, '运行号必须用 shortRunId()，不能再对 run_id 直接 slice');
assert.ok(/shortRunId\(runDetail\.run\.run_id\)/.test(workflowsPage), '工作流运行详情要复用 shortRunId');
assert.ok(/shortRunId\(selectedLiveRun\.run\.run_id\)/.test(schedulesPage), '计划详情要复用 shortRunId');

// 刚启动的运行可能还没落盘，详情首读会瞬时失败：必须自己重试，
// 不然「启动运行」之后用户看到的是红条错误而不是正在跑的任务。
assert.ok(/runDetailRetry/.test(workflowsPage), '运行详情首读失败要有重试预算');
assert.ok(/selectedRunStatus/.test(workflowsPage), '详情轮询要能用列表行状态兜底');

// 工作流中心的瞬时读失败同样不能把整页刷成红色错误条：保留上一份快照，
// 首读落空自己补一次重试，连续失败才提示。
const appShell = readFileSync(join(here, '..', 'src', 'main.tsx'), 'utf8');
assert.ok(!/catch \(error\) \{\s*setWorkflowCenter\(null\)/.test(appShell), '工作流中心读取失败时不能清空已有快照');
assert.ok(/workflowCenterSnapshot/.test(appShell), '工作流中心要记住上一份快照，用来判断是否已有数据可显示');
assert.ok(/workflowCenterFailures/.test(appShell), '工作流中心要有连续失败计数，避免单次抖动就报错');

// 来源管理的文案要收敛成短标签：卡片摘要、字段名、空状态都只留必要信息，
// 一旦回退成长句，弹窗会重新变成「说明书」。
const sourceDialog = readFileSync(join(here, '..', 'src', 'components', 'ExtensionSourcesDialog.tsx'), 'utf8');
const verboseSourceCopy = ['本机已有', '用右上角', 'GitHub 仓库链接', '该来源目录中的最新版本', '同名扩展由其他来源提供', '重新安装会改用本来源'];
for (const copy of verboseSourceCopy) {
  assert.ok(!sourceDialog.includes(copy), `来源管理不能再用长句文案「${copy}」`);
}
assert.ok(/本机 \$\{install\.installed\}\/\$\{group\.unit\.assets\.length\}/.test(sourceDialog), '来源卡片摘要要用「本机 N/M」这种短表达');

// 来源报错原文里带长 URL：卡片必须能收缩，否则弹窗出现横向滚动条，
// 卡片右侧的安装按钮会被裁到看不见。
const sourceErrorRule = styles.match(/\.extension-source-error \{[^}]*\}/);
assert.ok(sourceErrorRule && /min-width: 0/.test(sourceErrorRule[0]) && /max-width: 100%/.test(sourceErrorRule[0]), '来源报错行必须能收缩，不能被长 URL 撑宽');
for (const rule of ['.extension-source-unit {', '.extension-source-unit-members {', '.extension-source-member {']) {
  const match = styles.match(new RegExp(`${rule.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}[^}]*\\}`));
  assert.ok(match && /min-width: 0/.test(match[0]), `${rule} 要允许收缩，避免内容撑破来源卡片`);
}

// 「正在处理」只能有一个图形：刷新图标（RefreshCw）表示「这里可以点一下重取」，
// 一旦它又被当成加载指示用（旋转中的刷新图标），同一按钮就有了两种含义。
const tsxFiles = (function walk(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return walk(path);
    return /\.tsx$/.test(name) ? [path] : [];
  });
})(join(here, '..', 'src'));
const spinSmells = tsxFiles.filter((file) => /className="[^"]*\bspin\b/.test(readFileSync(file, 'utf8')));
assert.deepEqual(spinSmells, [], '加载指示必须用 <BusyIndicator>，不能在图标上挂 spin 类');
assert.ok(!/\.spinner \{[^}]*border-top-color/.test(styles), '手写的圆环 spinner 已被 BusyIndicator 取代');
assert.ok(existsSync(join(here, '..', 'src', 'components', 'BusyIndicator.tsx')), 'BusyIndicator 是全应用唯一的加载指示，不能删');
assert.ok(existsSync(join(here, '..', 'src', 'components', 'MorphIcon.tsx')), 'MorphIcon 是形变图标的唯一适配层，不能删');

// 任务中心的状态列从「小圆点」换成了状态图形：同一行换状态时图标在原地形变，
// 回退成圆点就只剩颜色一个维度，「加载中」和「已完成」在色弱视角下会难以区分。
const taskCenter = readFileSync(join(here, '..', 'src', 'pages', 'TaskCenterPage.tsx'), 'utf8');
assert.ok(/className=\{`task-center-state-icon/.test(taskCenter), '任务中心行首必须是状态图形');
assert.ok(!/task-center-status-dot/.test(taskCenter), '任务中心不能再回退到状态小圆点');
assert.ok(/function stateIcon\(status: string\)/.test(taskCenter), '任务中心的状态图形必须由状态推导');
// 这个选择器只能有一处，而且基础状态必须是「转」：拿到进度的运行行没有转圈就只剩颜色一个维度。
// 系统开启「减少动态效果」时由全局 reduce 分支统一降级，状态仍由文字和进度值表达。
const stateIconRules = styles.match(/\.task-center-state-icon\.is-spinning svg[^{}]*\{[^}]*\}/g) ?? [];
assert.ok(stateIconRules.some((rule) => /animation: spin/.test(rule)), '拿不到进度的运行行必须转圈，否则看不出「还活着」');
assert.ok(
  !stateIconRules.some((rule) => /animation: none|animation-name: (?!spin)/.test(rule)),
  '状态图形不能被降级成呼吸或静止：运行态必须始终在转',
);

// 快捷工具必须能承载大量插件入口：顶部只保留有限槽位，完整入口进入可搜索、
// 可收藏、可学习常用项的对话框，而不是无限横向堆叠或跳回插件管理页。
assert.match(shell, /QUICK_ACCESS_VISIBLE_LIMIT = 4/, '顶部快捷工具必须明确限制可见槽位');
assert.match(shell, /himind\.quick-access\.favorites/, '快捷工具收藏必须持久化');
assert.match(shell, /himind\.quick-access\.hidden/, '快捷栏隐藏状态必须持久化');
assert.match(shell, /onToggleHidden/, '全部工具面板必须提供隐藏/恢复快捷入口的操作');
assert.match(shell, /隐藏只影响快捷栏，不会卸载插件/, '隐藏操作必须明确说明不会卸载插件');
assert.match(shell, /himind\.quick-access\.usage/, '快捷工具常用频次必须持久化');
assert.match(shell, /useModalFocus\(dialogRef, true, onClose, searchRef, returnFocusRef\)/, '工具面板必须支持焦点接管与关闭回焦');
assert.match(shell, /aria-labelledby="quick-access-dialog-title"/, '工具面板必须有可访问名称');
assert.match(shell, /搜索工具、插件或视图/, '工具面板必须提供搜索入口');
assert.ok(!/打开更多插件工具/.test(shell), '超出槽位不能只跳回插件管理页，必须进入完整工具面板');
const quickDialogListRule = styles.match(/\.quick-access-dialog-list \{[^}]*\}/);
assert.ok(quickDialogListRule && /overflow-y: auto/.test(quickDialogListRule[0]), '大量工具必须由面板内部滚动，不能撑破窗口');
const quickFavoriteRule = styles.match(/\.quick-access-favorite-button, \.quick-access-hide-button \{[^}]*\}/);
assert.ok(quickFavoriteRule && /width: 36px; height: 36px/.test(quickFavoriteRule[0]), '收藏与隐藏按钮命中区必须至少 36×36');
console.log('shell structure checks passed');
