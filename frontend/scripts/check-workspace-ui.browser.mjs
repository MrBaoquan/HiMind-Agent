import assert from 'node:assert/strict';
import { readFileSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { createServer } from 'vite';
import react from '@vitejs/plugin-react';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const evidence = process.env.WORKSPACE_UI_EVIDENCE || resolve(root, '../target/workspace-ui-regression');
mkdirSync(evidence, { recursive: true });
rmSync(resolve(evidence, 'result.json'), { force: true });
// Written by source_wire_keys_match_filter_ids_and_accept_legacy_opencode Rust test.
const wireText = readFileSync(process.env.WORKSPACE_SOURCE_WIRE_FIXTURE || resolve(root, '../target/workspace-source-wire.json'), 'utf8');
const rustFixture = JSON.parse(wireText);
const wire = rustFixture.source_keys;
assert.deepEqual(wire, ['dsh', 'acp', 'claude_code', 'codex', 'opencode']);
assert.deepEqual(rustFixture.history.sources.map(s => s.source), wire);
assert.deepEqual(rustFixture.history.sessions.map(s => s.source), wire);
const require = createRequire(import.meta.url);
const { chromium } = process.env.PLAYWRIGHT_MODULE ? await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE).href) : require('playwright');
const server = await createServer({ root, cacheDir: resolve(evidence, 'vite-cache'), configFile: false, plugins: [react(), {
  name: 'workspace-ui-fixture',
  configureServer(vite) {
    vite.middlewares.use('/__workspace-source-wire', (_req, res) => { res.setHeader('Content-Type', 'application/json'); res.end(wireText); });
    vite.middlewares.use('/__workspace-ui', async (_req, res) => {
      res.setHeader('Content-Type', 'text/html; charset=utf-8');
      res.end(await vite.transformIndexHtml('/__workspace-ui', '<!doctype html><html lang="zh-CN"><head><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="icon" href="data:,"></head><body><div id="root"></div><script type="module" src="/scripts/fixtures/workspace-ui-review.tsx"></script></body></html>'));
    });
  },
}], server: { host: '127.0.0.1', port: 0 } });
await server.listen();
const browser = await chromium.launch({ headless: true, ...(process.env.CHROMIUM_EXECUTABLE ? { executablePath: process.env.CHROMIUM_EXECUTABLE } : {}) });
const page = await browser.newPage({ viewport: { width: 1024, height: 680 } });
const origin = `http://127.0.0.1:${server.httpServer.address().port}`;
const results = [];
const errors = [];
const measurements = { viewports: [], containers: [], diagnostics: [] };
page.on('pageerror', e => errors.push(e.message));
page.on('console', e => { if (e.type() === 'error') errors.push(e.text()); });
async function open(scenario = 'normal', wrapper = 'shell') {
  await page.goto(`${origin}/__workspace-ui?scenario=${scenario}&wrapper=${wrapper}`);
  await page.locator('.workspace-registry-head').waitFor();
  if (!['registry-error', 'empty', 'history-error'].includes(scenario)) await page.locator('.workspace-session-history-row').first().waitFor();
}
async function check(name, run) { await run(); results.push(name); console.log(`PASS ${name}`); }
async function insideDialog() { return page.evaluate(() => !!document.activeElement.closest('dialog[open]')); }
async function visiblePreview() {
  return page.locator('.workspace-session-history-preview-head h5').evaluate(e => { const r = e.getBoundingClientRect(); return r.top >= 0 && r.bottom <= innerHeight && document.activeElement === e; });
}
async function layout() {
  return page.evaluate(() => {
    const boxes = ['.workspace-session-history', '.workspace-session-history-tools', '.workspace-session-history-row', '.workspace-session-history-pagination', '.workspace-session-history-preview-head'];
    return boxes.flatMap(selector => [...document.querySelectorAll(selector)].filter(e => e.getClientRects().length && !e.closest('[hidden]')).flatMap(e => {
      const r = e.getBoundingClientRect();
      const overflow = e.scrollWidth > e.clientWidth + 1 ? [{ selector, scrollWidth:e.scrollWidth, clientWidth:e.clientWidth }] : [];
      return [...overflow, ...[...e.children].filter(child => !child.classList.contains('sr-only') && child.getClientRects().length).flatMap(child => {
        const c = child.getBoundingClientRect(); return c.right > r.right + 1 || c.left < r.left - 1 ? [{ selector, child: child.className, parent: r.width, width: c.width }] : [];
      })];
    }));
  });
}
try {
  await check('抽屉焦点、键盘循环、菜单命中及 Esc 回还', async () => {
    await open(); await page.getByRole('button', { name: '新建项目', exact: true }).click();
    const dialog = page.getByRole('dialog');
    assert.equal(await dialog.getByLabel('显示名称', { exact: true }).evaluate(e => e === document.activeElement), true);
    const hit = await dialog.getByRole('button', { name: '关闭', exact: true }).evaluate(e => { const r = e.getBoundingClientRect(); return [[2,2],[r.width-2,2],[2,r.height-2],[r.width-2,r.height-2],[r.width/2,r.height/2]].every(([x,y]) => e.contains(document.elementFromPoint(r.x+x, r.y+y))); });
    assert.equal(hit, true);
    for (const key of ['Tab','Shift+Tab']) for (let i = 0; i < 25; i++) { await page.keyboard.press(key); assert.equal(await insideDialog(), true); }
    await page.locator('.workspace-registry-head button').first().evaluate(e => e.focus()); assert.equal(await insideDialog(), true);
    await page.screenshot({path:resolve(evidence,'editor-1024.png')});
    await page.keyboard.press('Escape'); assert.equal(await dialog.count(), 0);
    assert.equal(await page.getByRole('button', { name: '新建项目', exact: true }).evaluate(e => e === document.activeElement), true);
  });
  await check('嵌套删除确认只关闭顶层并恢复删除按钮', async () => {
    await page.getByRole('button', { name: '编辑项目', exact: true }).click();
    await page.getByRole('dialog').getByRole('button', { name: '删除', exact: true }).click();
    assert.equal(await page.getByRole('dialog').count(), 2);
    assert.equal(await page.locator('.confirm-modal').getByRole('button', { name: '取消', exact: true }).evaluate(e => e === document.activeElement), true);
    await page.keyboard.press('Escape'); assert.equal(await page.getByRole('dialog').count(), 1);
    assert.equal(await page.getByRole('dialog').getByRole('button', { name: '删除', exact: true }).evaluate(e => e === document.activeElement), true);
    await page.keyboard.press('Escape');
  });
  await check('共享确认框兼容旧模态键盘管理', async () => {
    await open('legacy-modal');
    await page.getByRole('button', { name:'打开合成确认', exact:true }).click();
    for(let i=0;i<8;i++) {
      await page.keyboard.press('Tab');
      assert.equal(await page.evaluate(() => !!document.activeElement.closest('.confirm-modal')), true);
    }
    await page.keyboard.press('Escape'); assert.equal(await page.locator('.confirm-modal').count(),0);
    assert.equal(await page.locator('.legacy-modal-fixture').count(),1);
    assert.equal(await page.getByRole('button',{name:'打开合成确认',exact:true}).evaluate(e=>e===document.activeElement),true);
    await page.keyboard.press('Escape'); assert.equal(await page.locator('.legacy-modal-fixture').count(),0);
    await open();
  });
  await check('删除失败给出实际可执行动作', async () => {
    await page.getByRole('button', { name: '编辑项目', exact: true }).click();
    await page.getByRole('dialog').getByRole('button', { name: '删除', exact: true }).click();
    await page.locator('.confirm-modal').getByRole('button', { name: '删除', exact: true }).click();
    await page.getByRole('alert').waitFor(); assert.match(await page.getByRole('alert').textContent(), /调整子项目的上级/);
    assert.doesNotMatch(await page.getByRole('alert').textContent(), /强制/); await page.keyboard.press('Escape');
  });
  await check('保存失败在抽屉内宣布、保留输入并可重试', async () => {
    await open('save-error'); await page.getByRole('button', { name: '新建项目', exact: true }).click();
    await page.getByRole('dialog').getByLabel('显示名称', { exact: true }).fill('合成新项目');
    await page.getByRole('dialog').getByRole('button', { name: '新建项目', exact: true }).click();
    await page.getByRole('dialog').getByRole('alert').waitFor(); assert.match(await page.getByRole('alert').textContent(), /项目未保存/);
    await page.screenshot({path:resolve(evidence,'editor-error.png')});
    assert.equal(await page.getByRole('dialog').getByLabel('显示名称', { exact: true }).inputValue(), '合成新项目');
    await page.getByRole('dialog').getByRole('button', { name: '新建项目', exact: true }).click(); await page.getByRole('dialog').waitFor({ state: 'detached' });
    assert.equal(await page.evaluate(() => window.workspaceFixture.saves), 2);
    assert.equal(await page.getByRole('button', { name: '新建项目', exact: true }).evaluate(e => e === document.activeElement), true);
  });
  await check('提交期间阻止关闭及重复保存', async () => {
    await open('slow-save'); await page.getByRole('button', { name: '新建项目', exact: true }).click();
    await page.getByRole('dialog').getByLabel('显示名称', { exact: true }).fill('等待保存');
    await page.getByRole('dialog').getByRole('button', { name: '新建项目', exact: true }).click();
    await page.keyboard.press('Escape'); assert.equal(await page.getByRole('dialog').count(), 1);
    assert.equal(await page.getByRole('dialog').getByRole('button', { name: '关闭', exact: true }).isDisabled(), true);
    await page.getByRole('dialog').waitFor({ state: 'detached' }); assert.equal(await page.evaluate(() => window.workspaceFixture.saves), 1);
  });
  await check('目录选择失败可就近重试', async () => {
    await open('pick-error'); await page.getByRole('button', { name: '新建项目', exact: true }).click();
    const dialog = page.getByRole('dialog');
    await dialog.getByLabel('显示名称', { exact: true }).fill('选择目录失败');
    await dialog.getByRole('button', { name: '选择目录', exact: true }).click();
    assert.match(await dialog.getByRole('alert').textContent(), /目录未选择.*重试/);
    await dialog.getByRole('button', { name: '选择目录', exact: true }).click();
    assert.match(await dialog.locator('.workspace-registry-directory-picker code').textContent(), /synthetic\/chosen/);
    assert.equal(await dialog.getByLabel('显示名称', { exact: true }).inputValue(), '选择目录失败');
    await page.keyboard.press('Escape');
  });
  await check('首个目录冲突保留已保存项目，更换目录不重复创建', async () => {
    await open('bind-conflict'); await page.getByRole('button', { name: '新建项目', exact: true }).click();
    const dialog = page.getByRole('dialog');
    await dialog.getByLabel('显示名称', { exact: true }).fill('目录冲突项目');
    await dialog.getByRole('button', { name: '选择目录', exact: true }).click();
    await dialog.getByRole('button', { name: '保存并添加目录', exact: true }).click();
    assert.match(await dialog.getByRole('alert').textContent(), /项目已保存.*目录已关联其他项目.*解除原关联/);
    await dialog.getByRole('button', { name: '更换目录', exact: true }).click();
    await dialog.getByRole('button', { name: '保存并添加目录', exact: true }).click();
    await dialog.waitFor({ state: 'detached' });
    assert.deepEqual(await page.evaluate(() => window.workspaceFixture.saveIds), [null,'synthetic-new']);
  });
  await check('慢刷新立即反馈、防重复、完成后恢复', async () => {
    await open('slow-refresh'); const refresh = page.getByRole('button', { name: '刷新工作区', exact: true });
    await refresh.click(); assert.equal(await refresh.isDisabled(), true);
    assert.equal(await page.locator('.workspace-registry').getAttribute('aria-busy'), 'true');
    assert.match(await page.locator('.workspace-registry-status').textContent(), /正在刷新/);
    assert.equal(await page.getByRole('button', { name: '编辑项目', exact: true }).isDisabled(), true);
    assert.equal(await page.getByRole('button', { name: '新建项目', exact: true }).isDisabled(), true);
    await refresh.evaluate(e => { for (let i=0;i<5;i++) e.click(); });
    await page.waitForFunction(() => document.querySelector('.workspace-registry').getAttribute('aria-busy') === 'false');
    assert.equal(await page.evaluate(() => window.workspaceFixture.reads), 2); assert.match(await page.locator('.workspace-registry-status').textContent(), /已更新/);
  });
  await check('登记读取错误可重试', async () => {
    await open('registry-error'); await page.getByRole('alert').waitFor(); assert.match(await page.getByRole('alert').textContent(), /读取失败/);
    await page.getByRole('button', { name: '重新加载', exact: true }).click(); await page.locator('.workspace-session-history-row').first().waitFor();
  });
  await check('历史游标错误刷新后恢复', async () => {
    await open('history-error'); await page.getByRole('alert').waitFor(); assert.match(await page.getByRole('alert').textContent(), /列表已变化/);
    await page.getByRole('button', { name: '刷新列表', exact: true }).click(); await page.locator('.workspace-session-history-row').first().waitFor();
  });
  await check('关联目录必须明确确认', async () => {
    await open(); await page.getByRole('tab', { name: /待确认目录/ }).click();
    await page.locator('.workspace-registry-candidate-actions select').selectOption('p1');
    assert.equal(await page.evaluate(() => window.workspaceFixture.binds), 0);
    await page.locator('.workspace-registry-candidate-actions').getByRole('button', { name: '添加目录', exact: true }).click();
    assert.equal(await page.evaluate(() => window.workspaceFixture.binds), 1);
  });
  await check('50 条列表首尾与单条预览立即可见，返回恢复焦点和滚动', async () => {
    for (const [scenario, index, keyboard] of [['normal',0,false], ['normal',49,false], ['one-session',0,false], ['normal',0,true], ['normal',49,true], ['one-session',0,true], ['slow-transcript',0,true]]) {
      await open(scenario); const trigger = page.getByRole('button', { name: /^预览会话：/ }).nth(index);
      await trigger.scrollIntoViewIfNeeded(); const before = await page.locator('.main-content').evaluate(e => e.scrollTop);
      const label = await trigger.getAttribute('aria-label');
      if(keyboard) { await trigger.focus(); await page.keyboard.press('Enter'); } else await trigger.click();
      assert.equal(await visiblePreview(), true);
      if(scenario === 'slow-transcript') assert.equal(await page.getByRole('status').filter({hasText:'读取消息页'}).isVisible(), true);
      await page.locator('.workspace-session-history-message').first().waitFor();
      if(scenario === 'normal' && index === 0) await page.screenshot({path:resolve(evidence,'preview-after-click.png')});
      assert.equal(await page.locator('.workspace-session-history-results').isVisible(), false);
      await page.keyboard.press('Escape'); assert.equal(await page.locator('.workspace-session-history-preview').count(), 0);
      assert.equal(await page.evaluate(() => document.activeElement.getAttribute('aria-label')), label);
      assert.ok(Math.abs(await page.locator('.main-content').evaluate(e => e.scrollTop) - before) <= 2);
    }
  });
  await check('正文读取失败可重试且保留详情位置', async () => {
    await open('transcript-error'); await page.getByRole('button', { name: /^预览会话：/ }).first().click();
    await page.getByRole('alert').waitFor(); await page.getByRole('button', { name: '重试', exact: true }).click();
    await page.locator('.workspace-session-history-message').first().waitFor();
    await page.getByRole('button', { name: '关闭预览', exact: true }).click();
  });
  await check('历史滚动只移动列表，页头、项目树与分页保持可见', async () => {
    await page.setViewportSize({width:1024,height:680}); await open();
    const list = page.getByRole('list',{name:'会话列表',exact:true});
    const chromeBefore = await page.locator('.workspace-registry-head').evaluate(e=>e.getBoundingClientRect().top);
    const footerBefore = await page.locator('.workspace-session-history-results > footer').evaluate(e=>e.getBoundingClientRect().top);
    await list.hover(); await page.mouse.wheel(0,700);
    await page.waitForFunction(()=>document.querySelector('.workspace-session-history-list').scrollTop > 0);
    const scroll = await list.evaluate(e=>e.scrollTop);
    assert.equal(await page.locator('.main-content').evaluate(e=>e.scrollTop),0);
    assert.equal(await page.locator('.workspace-registry-head').evaluate(e=>e.getBoundingClientRect().top),chromeBefore);
    assert.equal(await page.locator('.workspace-session-history-results > footer').evaluate(e=>e.getBoundingClientRect().top),footerBefore);
    await page.getByRole('button',{name:/^项目目录/}).click();
    await page.getByRole('button',{name:'会话历史',exact:true}).click();
    assert.ok(Math.abs(await list.evaluate(e=>e.scrollTop)-scroll)<=2);
    await list.focus(); await page.keyboard.press('PageDown');
    await page.waitForFunction(previous=>document.querySelector('.workspace-session-history-list').scrollTop > previous, scroll);
    assert.equal(await page.locator('.main-content').evaluate(e=>e.scrollTop),0);
    await list.evaluate(e=>e.scrollTop=e.scrollHeight); await list.hover(); await page.mouse.wheel(0,400);
    assert.equal(await page.locator('.main-content').evaluate(e=>e.scrollTop),0);
  });
  await check('长项目树与目录独立滚动，搜索和当前项目保持可达', async () => {
    await open('many-projects');
    const tree=page.getByRole('list',{name:'项目列表',exact:true});
    await tree.hover(); await page.mouse.wheel(0,600);
    await page.waitForFunction(()=>document.querySelector('.workspace-registry-tree').scrollTop > 0);
    assert.equal(await page.locator('.workspace-session-history-list').evaluate(e=>e.scrollTop),0);
    assert.equal(await page.getByRole('searchbox',{name:'搜索项目、别名或目录',exact:true}).isVisible(),true);
    await page.getByRole('button',{name:/^项目目录/}).click();
    const dirs=page.locator('.workspace-registry-detail-section');
    await dirs.hover(); await page.mouse.wheel(0,700);
    await page.waitForFunction(()=>document.querySelector('.workspace-registry-detail-section').scrollTop > 0);
    assert.equal(await page.locator('.workspace-registry-detail-head').isVisible(),true);
    assert.equal(await page.locator('.main-content').evaluate(e=>e.scrollTop),0);
  });
  await check('刷新保留阅读位置，筛选从页首开始，等待期间禁止预览旧结果', async () => {
    await open('slow-history');
    const list=page.getByRole('list',{name:'会话列表',exact:true});
    await list.hover(); await page.mouse.wheel(0,600);
    await page.waitForFunction(()=>document.querySelector('.workspace-session-history-list').scrollTop>0);
    const before=await list.evaluate(e=>e.scrollTop);
    await page.getByRole('button',{name:'刷新会话历史',exact:true}).click();
    await page.getByRole('status').filter({hasText:'更新中'}).waitFor();
    assert.equal(await page.getByRole('button',{name:/^预览会话/}).first().isDisabled(),true);
    await page.waitForFunction(()=>document.querySelector('.workspace-session-history').getAttribute('aria-busy')==='false');
    assert.ok(Math.abs(await list.evaluate(e=>e.scrollTop)-before)<=2);
    await page.getByRole('combobox',{name:'按来源筛选',exact:true}).selectOption('codex');
    await page.waitForFunction(()=>document.querySelector('.workspace-session-history').getAttribute('aria-busy')==='false' && document.querySelectorAll('.workspace-session-history-row').length===11);
    assert.equal(await list.evaluate(e=>e.scrollTop),0);
    assert.equal(await page.locator('.main-content').evaluate(e=>e.scrollTop),0);
  });
  await check('展开来源诊断后保留可读列表和固定分页', async () => {
    for (const width of [1024,768]) {
      await page.setViewportSize({width,height:680}); await open();
      await page.locator('.workspace-session-history-source-details > summary').click();
      await page.getByText('来源状态与恢复建议',{exact:true}).click();
      await page.getByText('诊断详情',{exact:true}).click();
      const sizes=await page.evaluate(()=>({
        width:innerWidth,
        diagnostics:document.querySelector('.workspace-session-history-source-details').getBoundingClientRect().height,
        list:document.querySelector('.workspace-session-history-list').getBoundingClientRect().height,
        footerBottom:document.querySelector('.workspace-session-history-results > footer').getBoundingClientRect().bottom,
        viewport:innerHeight,
      }));
      measurements.diagnostics.push(sizes);
      assert.ok(sizes.diagnostics<=181 && sizes.list>96 && sizes.footerBottom<=sizes.viewport,JSON.stringify(sizes));
      await page.screenshot({path:resolve(evidence,`source-diagnostics-${width}.png`)});
    }
    await page.setViewportSize({width:1024,height:680});
  });
  await check('正文与列表共用固定空间，返回恢复列表位置，翻页从页首阅读', async () => {
    await open(); const list=page.locator('.workspace-session-history-list');
    const trigger=page.getByRole('button',{name:/^预览会话/}).nth(30);
    await trigger.scrollIntoViewIfNeeded(); const previous=await list.evaluate(e=>e.scrollTop);
    await trigger.click(); const messages=page.getByRole('list',{name:'会话消息',exact:true});
    await messages.waitFor(); await messages.hover(); await page.mouse.wheel(0,500);
    await page.waitForFunction(()=>document.querySelector('.workspace-session-history-messages').scrollTop>0);
    assert.equal(await page.locator('.main-content').evaluate(e=>e.scrollTop),0);
    assert.equal(await page.getByRole('button',{name:'关闭预览',exact:true}).isVisible(),true);
    await page.getByRole('button',{name:'更早消息',exact:true}).click();
    await page.waitForFunction(()=>document.querySelector('.workspace-session-history-messages').scrollTop===0);
    await page.getByRole('button',{name:'关闭预览',exact:true}).click();
    assert.ok(Math.abs(await list.evaluate(e=>e.scrollTop)-previous)<=2);
    await page.getByRole('button',{name:'下一页',exact:true}).click();
    await page.waitForFunction(()=>document.querySelectorAll('.workspace-session-history-row').length===5);
    assert.equal(await list.evaluate(e=>e.scrollTop),0);
  });
  await check('打开目录复用真实入口并支持复制路径', async () => {
    await open(); await page.context().grantPermissions(['clipboard-read','clipboard-write']);
    await page.locator('.workspace-registry-detail-head').getByRole('button',{name:'打开目录',exact:true}).click();
    assert.equal(await page.locator('.workspace-registry-detail-section').isVisible(),true);
    const item=page.locator('.workspace-registry-detail-paths > li').first();
    const path=await item.locator('code').textContent();
    await item.getByRole('button',{name:'打开目录',exact:true}).click();
    assert.deepEqual(await page.evaluate(()=>window.workspaceFixture.openedPaths),[path]);
    await item.getByRole('button',{name:'复制路径',exact:true}).click();
    assert.equal(await page.evaluate(()=>navigator.clipboard.readText()),path);
    await page.getByRole('button',{name:'会话历史',exact:true}).click();
    await page.getByRole('button',{name:/^预览会话/}).first().click();
    await page.setViewportSize({width:768,height:680});
    assert.equal(await page.getByRole('combobox',{name:'切换项目',exact:true}).isVisible(),true);
    await page.getByRole('combobox',{name:'切换项目',exact:true}).selectOption('p3');
    await page.locator('.workspace-session-history-empty').waitFor();
    assert.equal(await page.locator('.workspace-session-history-preview').count(),0);
    await page.setViewportSize({width:1024,height:680});
  });
  await check('Rust 实际序列化五来源贯穿状态、筛选、列表和正文', async () => {
    await open(); const labels=['HiMind AI','ACP','Claude Code','Codex','OpenCode'];
    for (let i=0;i<wire.length;i++) {
      await page.getByRole('combobox', { name: '按来源筛选' }).selectOption(wire[i]);
      await page.waitForFunction(label => { const rows = [...document.querySelectorAll('.workspace-session-history-source')]; return rows.length > 0 && rows.every(e => e.textContent === label); }, labels[i]);
      assert.ok((await page.locator('.workspace-session-history-source-state').allTextContents()).some(t => t.includes(labels[i])));
      await page.getByRole('button', { name: /^预览会话：/ }).first().click(); await page.locator('.workspace-session-history-message.assistant').first().waitFor();
      assert.equal(await page.locator('.workspace-session-history-message.assistant strong').first().textContent(), labels[i]);
      await page.getByRole('button', { name: '关闭预览', exact: true }).click();
    }
    for (const scenario of ['legacy-source','unknown-source']) { await open(scenario); assert.doesNotMatch(await page.locator('.workspace-session-history-sources').textContent(), /undefined/); }
  });
  await check('目录链接命中区及工作区字阶', async () => {
    await open(); const fonts = await page.evaluate(() => [
      ['.workspace-registry-node-copy small',12],['.workspace-registry-node-count',12],['.workspace-registry-path',12],
      ['.workspace-session-history-row-meta',12],['.workspace-session-history-row-main code',12],['.workspace-session-history-row-main strong',13],['.workspace-session-history-title h4',14],
    ].map(([selector,min]) => ({selector,min,size:parseFloat(getComputedStyle(document.querySelector(selector)).fontSize)})));
    assert.ok(fonts.every(f => f.size>=f.min), JSON.stringify(fonts));
    const heights = await page.locator('.workspace-registry-node-select').evaluateAll(es => es.map(e => e.getBoundingClientRect().height));
    assert.ok(heights.every(h => h <= 120), `project rows stretched: ${heights}`);
    const toggle = page.getByRole('button', { name: '收起审查项目的子项目' });
    await toggle.click(); assert.equal(await page.locator('.workspace-registry-tree-children').isVisible(), false);
    await page.getByRole('button', { name: '展开审查项目的子项目' }).click();
    assert.equal(await page.locator('.workspace-registry-tree-children').isVisible(), true);
    await page.getByRole('tab', { name: /目录清单/ }).click();
    const targets = await page.locator('.workspace-registry-project-link').evaluateAll(es => es.map(e => ({w:e.getBoundingClientRect().width,h:e.getBoundingClientRect().height})));
    assert.ok(targets.every(t => t.w>=24 && t.h>=24));
  });
  await check('宽度矩阵与 240–660px 历史容器无操作越界', async () => {
    for (const width of [1280,1024,768,560,400,320]) {
      await page.setViewportSize({width,height:820}); await open('normal',width>=768?'shell':'isolated');
      assert.deepEqual(await layout(), [], `viewport ${width}`);
      const viewport = await page.evaluate(() => ({ width:innerWidth, scrollWidth:document.documentElement.scrollWidth }));
      measurements.viewports.push(viewport); assert.ok(viewport.scrollWidth <= viewport.width + 1, JSON.stringify(viewport));
      if(width>=768) await page.screenshot({path:resolve(evidence,`workspace-${width}.png`)});
    }
    await page.setViewportSize({width:1024,height:900}); await open('normal','isolated');
    for (const width of [240,280,320,400,500,660]) {
      await page.locator('.workspace-session-history').evaluate((e,w) => { e.style.flex='0 0 auto'; e.style.width=`${w}px`; e.style.maxWidth='none'; },width);
      assert.deepEqual(await layout(), [], `container ${width}`);
      const measured=await page.locator('.workspace-session-history').evaluate(e => ({width:e.clientWidth,scrollWidth:e.scrollWidth}));
      assert.ok(Math.abs(measured.width-width)<=1,JSON.stringify({requested:width,...measured}));
      measurements.containers.push(measured);
      await page.getByRole('button', { name: /^预览会话：/ }).first().click();
      await page.locator('.workspace-session-history-message').first().waitFor();
      assert.deepEqual(await layout(), [], `preview container ${width}`);
      await page.getByRole('button', { name: '关闭预览', exact: true }).click();
    }
    await page.locator('.workspace-session-history').evaluate(e => {e.style.flex='';e.style.width='';e.style.maxWidth='';});
    await page.evaluate(() => { const s=document.createElement('style');s.textContent='*{line-height:1.5!important;letter-spacing:.12em!important;word-spacing:.16em!important}p{margin-block-end:2em!important}';document.head.append(s); });
    assert.deepEqual(await layout(), [], 'text spacing');
  });
  await check('空态与筛选无结果保留操作路径', async () => {
    await open('empty'); assert.equal(await page.locator('.workspace-registry-no-projects').getByRole('button',{name:'新建项目'}).count(),1);
    await open(); await page.getByRole('searchbox',{name:'搜索会话标题或目录',exact:true}).fill('not-present');
    await page.locator('.workspace-session-history-empty').waitFor(); assert.match(await page.locator('.workspace-session-history-empty').textContent(), /没有匹配/);
  });
  assert.deepEqual(errors, []);
  writeFileSync(resolve(evidence,'result.json'),JSON.stringify({passed:results.length,checks:results,wire,measurements,errors},null,2));
  console.log(`Workspace UI: ${results.length} checks passed`);
} catch(error) {
  await page.screenshot({path:resolve(evidence,'failure.png')});
  const geometry = await page.evaluate(() => {
    const tools = document.querySelector('.workspace-session-history-tools');
    if(!tools) return null;
    const parent = tools.getBoundingClientRect();
    return {width:innerWidth,grid:getComputedStyle(tools).gridTemplateColumns,parent:parent.toJSON(),children:[...tools.querySelectorAll('*')].map(e => ({tag:e.tagName,className:e.getAttribute('class'),rect:e.getBoundingClientRect().toJSON(),position:getComputedStyle(e).position,scrollWidth:e.scrollWidth,clientWidth:e.clientWidth}))};
  });
  writeFileSync(resolve(evidence,'failure.json'),JSON.stringify({passed:results,error:error.stack,errors,geometry},null,2));
  throw error;
} finally { await browser.close(); await server.close(); }
