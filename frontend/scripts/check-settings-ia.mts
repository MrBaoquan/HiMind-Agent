// 设置窗口信息架构的回归自检（零依赖：node --experimental-strip-types）。
// 左栏收敛过一次（12 条 → 3 组 8 条），这里锁住那次结论，防止菜单再长回去，
// 同时锁住「旧深链不能静默回落」这条已经实测踩过的坑。
import { strict as assert } from 'node:assert';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const readFrontend = (relative: string) => readFileSync(join(here, '..', relative), 'utf8');
const model = readFrontend(join('src', 'settingsModel.ts'));
const rail = readFrontend(join('src', 'components', 'SettingsWindow.tsx'));
const settingsPage = readFrontend(join('src', 'pages', 'SettingsPage.tsx'));
const main = readFrontend(join('src', 'main.tsx'));
const styles = readFrontend('styles.css');
const uiRs = readFileSync(join(here, '..', '..', 'src', 'app', 'ui.rs'), 'utf8');

// 1. 左栏规模上限：3 组、每组最多 3 条、合计最多 8 条。
//    超过这个数就该合并成条目内的页签，而不是往左栏再加一行。
const groups = [...model.matchAll(/label: '([^']+)',\s*\n\s*items: \[([\s\S]*?)\n\s*\],\n\s*\}/g)]
  .map(match => ({ label: match[1], items: (match[2].match(/key: '/g) || []).length }));
assert.ok(groups.length >= 1, 'settingsModel.ts 必须导出分组的左栏定义');
const railKeys = [...model.matchAll(/key: '(accounts|services|automation|approval|general|tooling|diagnostics|ai)'/g)].map(m => m[1]);
const totalItems = groups.reduce((sum, group) => sum + group.items, 0);
assert.ok(totalItems <= 8, `设置左栏合计不得超过 8 条，当前 ${totalItems} 条：${groups.map(g => `${g.label}=${g.items}`).join('、')}`);
for (const group of groups) {
  assert.ok(group.items <= 3, `设置左栏每组不得超过 3 条，「${group.label}」当前 ${group.items} 条`);
}
assert.equal(new Set(railKeys).size, railKeys.length, '设置左栏 key 不能重复');

// 2. 条目里的板块只能用页签表达，不能各占一行导航。
const tabs = model.match(/const SETTINGS_SECTION_TABS[\s\S]*?\n\};/);
assert.ok(tabs, 'settingsModel.ts 必须声明 SETTINGS_SECTION_TABS');
assert.ok(/settingsSectionTabs/.test(model), '页签要通过 settingsSectionTabs() 读取');
assert.ok(/role="tablist"/.test(settingsPage) && /role="tab"/.test(settingsPage), '设置页页签要有 tablist/tab 语义');

// 3. 每个左栏条目都要有渲染分支，否则点了会是一片空白。
for (const section of ['accounts', 'services', 'automation', 'approval', 'general', 'tooling', 'diagnostics']) {
  assert.ok(
    new RegExp(`section === '${section}'`).test(settingsPage),
    `SettingsPage 缺少「${section}」分支`,
  );
}

// 4. 旧深链必须映射到新键，不能靠后端静默回落「通用」。
const legacy = model.match(/const LEGACY_SETTINGS_SECTIONS[\s\S]*?\n\};/);
assert.ok(legacy, 'settingsModel.ts 必须声明 LEGACY_SETTINGS_SECTIONS');
for (const key of ['remote', 'connectors', 'remote-tools', 'tools', 'skills', 'backup', 'logs']) {
  assert.ok(new RegExp(`'?${key}'?:\\s*\\{`).test(legacy[0]), `旧深链 ${key} 缺少映射`);
}

// 5. 后端白名单必须覆盖前端的全部 section 与旧键，否则深链在 Rust 侧就被丢掉。
const openSettings = uiRs.match(/pub\(crate\) fn open_settings_window[\s\S]*?\n\}/);
assert.ok(openSettings, 'src/app/ui.rs 必须保留 open_settings_window');
const allowed = new Set([...openSettings[0].matchAll(/"([a-z-]+)"/g)].map(m => m[1]));
for (const section of ['accounts', 'services', 'automation', 'approval', 'general', 'tooling', 'diagnostics']) {
  assert.ok(allowed.has(section), `后端 section 白名单缺少新键「${section}」`);
}
for (const key of ['remote', 'connectors', 'remote-tools', 'tools', 'skills', 'backup', 'logs']) {
  assert.ok(allowed.has(key), `后端 section 白名单缺少旧键「${key}」，旧深链会被静默丢弃`);
}
assert.ok(/"tab"/.test(openSettings[0]), '后端要透传 tab，否则页签深链落不到对应板块');
assert.ok(/inner_size\(980\.0, 720\.0\)/.test(openSettings[0]) && /min_inner_size\(760\.0, 520\.0\)/.test(openSettings[0]), '设置窗口默认 980×720、最小 760×520 的约定不能被改回去');

// 6. 左栏可检索：8 条以后用户的词和菜单的词对不上，靠别名而不是把菜单拆回去。
assert.ok(/type="search"/.test(rail), '设置左栏必须有搜索框');
assert.ok(/settingsRailItemMatches/.test(rail) && /keywords/.test(model), '左栏检索要走 keywords 别名');
// 只命中别名的条目没有 <mark> 可画，必须回一行来源说明，否则用户看不出为什么命中。
assert.ok(/settingsRailItemMatch\(item, filter\)/.test(rail), '别名命中要在条目标题下回一行来源说明');
assert.ok(/\.settings-rail-alias \{/.test(styles), 'styles.css 必须有 .settings-rail-alias 样式');

// 7. 页头下沿只有一套间距（16px），页签视觉与 AI 连接的 .ai-tabs 对齐。
const paneHeader = styles.match(/\.settings-window-pane > \.page-header \{[^}]*\}/);
assert.ok(paneHeader && /margin-bottom: 16px/.test(paneHeader[0]), '设置内容页头下沿要保持 16px');
const aiHeader = styles.match(/\.ai-page \.page-header \{[^}]*\}/);
assert.ok(aiHeader && /margin-bottom: 16px/.test(aiHeader[0]), 'AI 连接页头下沿要和设置页统一成 16px');
assert.ok(/\.settings-tabs \{/.test(styles), 'styles.css 必须有 .settings-tabs 样式');

// 8. 分组标签的对比度：--text-subtle(#64748b) 在 --surface-muted(#f4f5f7) 上只有 4.36:1，
//    低于 11px 正文需要的 4.5:1，必须用 --text-muted。
const groupLabel = styles.match(/\.settings-rail-group-label \{[^}]*\}/);
assert.ok(groupLabel, 'styles.css 必须保留 .settings-rail-group-label');
assert.ok(/color: var\(--text-muted\)/.test(groupLabel[0]), '分组标签要用 --text-muted，--text-subtle 对比度不达标');

// 9. 设置窗口里的运行日志是嵌入页签，不能再套一层整屏高度。
assert.ok(/\.settings-window-pane \.logs-page\.embedded \{[^}]*height: auto/.test(styles), '嵌入的日志页要退出 height:100% 口径');
assert.ok(/\.settings-window-pane \.logs-page\.embedded \.log-list \{[^}]*max-height/.test(styles), '嵌入的日志列表要自己限高');

// 10. 窄窗（≤760px）左栏转横排必须写在媒体查询里。
//     左栏是 .settings-window-content 的兄弟节点，容器查询 agent-main 只作用于内容列内部，
//     曾经把这段写进 @container agent-main，结果一行都没生效（760×520 实测仍是竖栏）。
const cssBlocks = (source: string, header: string) => {
  const out: string[] = [];
  for (let i = source.indexOf(header); i >= 0;) {
    let depth = 0;
    let end = -1;
    for (let k = source.indexOf('{', i); k < source.length; k += 1) {
      if (source[k] === '{') depth += 1;
      else if (source[k] === '}') {
        depth -= 1;
        if (depth === 0) { end = k; break; }
      }
    }
    if (end < 0) break;
    out.push(source.slice(i, end + 1));
    i = source.indexOf(header, end);
  }
  return out;
};
const mediaBlocks = cssBlocks(styles, '@media (max-width: 760px)');
assert.ok(
  mediaBlocks.some(block => /\.settings-rail \{[^}]*flex-direction: row/.test(block)),
  '窄窗左栏转横排要写在 @media (max-width: 760px) 里',
);
for (const block of cssBlocks(styles, '@container agent-main')) {
  assert.ok(!/\.settings-rail\s*[,{]/.test(block), '左栏是 .settings-window-content 的兄弟节点，@container agent-main 覆盖不到 .settings-rail');
}

// 11. Chromium 会拒绝「不属于任何表单的密码框」（控制台 DOM 告警），而且口令是这一屏的主输入，
//     包进 <form> 之后回车提交给「导出备份包」才是自然动作。两条收益用一条断言锁住。
const backupPane = settingsPage.slice(
  settingsPage.indexOf("section === 'diagnostics' && activeTab === 'backup'"),
  settingsPage.indexOf('backupExportReport ?'),
);
assert.ok(backupPane.length > 0, 'SettingsPage 必须保留备份与恢复面板');
const formOpen = backupPane.indexOf('<form');
const formClose = backupPane.indexOf('</form>');
const passwordInput = backupPane.indexOf('type="password"');
assert.ok(formOpen >= 0 && formClose > formOpen, '备份口令输入必须包在 <form> 里，否则 Chromium 报「密码框不属于任何表单」');
assert.ok(passwordInput > formOpen && passwordInput < formClose, '备份口令 input 要落在 <form> 开闭之间');
assert.ok(/type="submit"[\s\S]{0,240}导出备份包/.test(backupPane), '「导出备份包」要是 form 的提交按钮，回车才有落点');

// 12. 账号页的 SVN 账号是本机用户自己选的、和工作台登录身份无关的本地配置。
//     设置窗口是独立入口，不会跑主窗口的 refreshDashboardIdentity，所以账号页的数据
//     加载必须自己带上 refreshSvnConnections；漏掉就会出现「文件里已配置、界面显示待配置」。
const settingsDataLoader = main.slice(
  main.indexOf('async function refreshSettingsPageData()'),
  main.indexOf('async function refreshLogs('),
);
assert.ok(settingsDataLoader.length > 0, 'main.tsx 必须保留 refreshSettingsPageData');
assert.ok(
  /refreshSvnConnections\(\)/.test(settingsDataLoader),
  '账号设置页加载数据时要一起读本机 SVN 账号，否则设置窗口会把已配置的账号显示成「待配置」',
);
assert.ok(
  /refreshSettingsPageData\(\)/.test(main.slice(main.indexOf("page === 'settings'"), main.indexOf("page === 'settings'") + 1200)),
  '进入设置页必须走 refreshSettingsPageData',
);

// 13. AI 连接是这套左栏里唯一一个「页签套页签」的面板，三条最容易退化的约定单独锁住：
//     · 页头说明不能丢（这句话同时是左栏条目的说明，只有这里两处共用）；
//     · 指路文案里的页签名要取自 aiConnectionsTabs，不能再出现「模型服务」这种早已改名的词；
//     · 没有可用来源时，「模型」接线要给下一步，不能只剩一个禁用按钮。
const aiPage = readFrontend(join('src', 'pages', 'AiConnectionsPage.tsx'));
const aiTabs = readFrontend(join('src', 'pages', 'aiConnectionsTabs.ts'));
const aiProviders = readFrontend(join('src', 'pages', 'AiProvidersPanel.tsx'));
const aiTools = readFrontend(join('src', 'pages', 'AiToolsPanel.tsx'));
const builtinAi = readFrontend(join('src', 'pages', 'BuiltinAiPage.tsx'));
assert.ok(aiTabs.includes("services: '模型来源'"), 'aiConnectionsTabs 必须声明「模型来源」这个页签标签');
assert.ok(/<PageHeader title="AI 连接" description=\{AI_CONNECTIONS_DESCRIPTION\}/.test(aiPage), 'AI 连接页头要带说明，与其他设置面板一致');
assert.ok(/AI_CONNECTIONS_TAB_LABELS\.services/.test(builtinAi), '对话页指路到模型来源页签时要取同一个标签常量，不能自己写死一个词');
assert.ok(!/media\.[a-z]/.test(aiProviders), '用户可见文案里不能出现 media.* 这类内部能力 ID');
assert.ok(/hasUsableSource/.test(aiTools) && aiTools.includes('还没有可用的模型来源'), '「模型」接线在一条可用来源都没有时要说明缺什么');
assert.ok(/onOpenModelSources=\{\(\) => setActiveTab\('services'\)\}/.test(aiPage), '「新增模型来源」要能直接切到模型来源页签');
assert.ok(/hasAnySource/.test(aiTools), '一条来源都没有时，工具接入页要在列表之前说明并给出入口');
assert.ok(/ai-service-media/.test(aiProviders), '媒体映射要收进可折叠块，首屏只留一行对话来源');
assert.ok(!/applyPreset\(group\.items\[0\]\.id\)/.test(aiProviders), '预设分类页签只切换列表，不能顺手替用户选中第一个厂商');
assert.ok(/ai-service-delete-note/.test(aiProviders), '不能删除的原因要写成看得见的字（禁用按钮的 title 在 Chromium 里弹不出来）');
assert.ok(/missing === 'Base URL'/.test(aiProviders), '缺字段要由 saveService 点名，按钮不能靠 disabled 把用户关在门外');

console.log(`settings IA: ${groups.length} 组 / ${totalItems} 条，深链白名单、窄窗折叠、AI 连接约定校验通过`);
