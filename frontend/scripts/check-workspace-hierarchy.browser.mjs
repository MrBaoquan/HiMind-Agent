import assert from 'node:assert/strict';
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { createServer } from 'vite';
import react from '@vitejs/plugin-react';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const evidence = process.env.HIERARCHY_EVIDENCE || resolve(root, '../target/workspace-hierarchy-browser');
mkdirSync(evidence, { recursive: true });
const { chromium } = await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE || resolve(root, '../target/session-history-playwright/node_modules/playwright/index.mjs')).href);
const server = await createServer({ root, configFile: false, cacheDir: resolve(evidence, 'vite-cache'), plugins: [react(), {
  name: 'hierarchy-fixture', configureServer(vite) { vite.middlewares.use('/__workspace-hierarchy', async (_req, res) => {
    res.setHeader('Content-Type', 'text/html; charset=utf-8');
    res.end(await vite.transformIndexHtml('/__workspace-hierarchy', '<!doctype html><html lang="zh-CN"><head><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="icon" href="data:,"></head><body><div id="root"></div><script type="module" src="/scripts/fixtures/workspace-hierarchy.tsx"></script></body></html>'));
  }); },
}], server: { host: '127.0.0.1', port: 0 } });
await server.listen();
const browser = await chromium.launch({ headless: true, executablePath: process.env.CHROMIUM_EXECUTABLE || 'C:/Program Files/Google/Chrome/Application/chrome.exe' });
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
await page.addInitScript(() => { Object.defineProperty(navigator, 'clipboard', { value: { writeText: async text => { window.fixtureCopied = text; } } }); });
const errors = [], results = [], measurements = [];
page.on('pageerror', error => errors.push(error.message));
page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
const url = `http://127.0.0.1:${server.httpServer.address().port}/__workspace-hierarchy`;
const title = page.locator('#workspace-selected-project-title');
const children = page.locator('.workspace-registry-child-list');
async function check(name, run) { await run(); results.push(name); console.log(`PASS ${name}`); }
async function history() { await page.getByRole('button', { name: '会话历史', exact: true }).click(); await page.locator('.workspace-session-history-row').first().waitFor(); }
async function select(id) {
  if (await page.getByRole('combobox', { name: '切换项目', exact: true }).isVisible()) await page.getByRole('combobox', { name: '切换项目', exact: true }).selectOption(id);
  else await page.locator('.workspace-registry-node-select').filter({ has: page.locator('strong', { hasText: id === 'root' ? /^IO 设备平台$/ : id === 'cinema' ? /^动感影院播控$/ : /^TechnicalTool$/ }) }).click();
}
try {
  await page.goto(url); await children.waitFor();
  await page.screenshot({ path: resolve(evidence, 'hierarchy-children-1280.png') });
  await check('中文别名导航、单行层级与详情目录计数', async () => {
    assert.equal(await title.textContent(), 'IO 设备平台');
    assert.equal(await children.locator('li').count(), 5);
    assert.match(await children.innerText(), /设备配置与编排/);
    assert.match(await page.locator('.workspace-registry-browser-head').innerText(), /主项目 2 · 子项目 5/);
    const navigation = page.getByRole('complementary', { name: '项目层级' });
    assert.doesNotMatch(await navigation.innerText(), /CinemaControl|IOStudio|直属目录|暂无直属目录|未添加目录/);
    const cinema = navigation.locator('.workspace-registry-node-select').filter({ hasText: '动感影院播控' });
    assert.match(await cinema.getAttribute('title'), /CinemaControl/);
    const rows = await navigation.locator('.workspace-registry-node-select').evaluateAll(nodes => nodes.map(node => node.getBoundingClientRect().height));
    assert.ok(rows.every(height => height >= 32 && height <= 44), `navigation row heights: ${rows}`);
    assert.equal(await page.evaluate(() => window.hierarchyFixture.calls.filter(c => c.kind === 'list').length), 0);
  });
  await check('技术名搜索保留祖先、进入中文子项目与返回父级', async () => {
    const search = page.getByRole('searchbox', { name: '搜索项目、别名或目录' });
    await search.fill('CinemaControl');
    assert.equal(await page.locator('.workspace-registry-node-select:visible').count(), 2);
    assert.equal(await page.locator('.workspace-registry-node-count').first().textContent(), '5');
    await select('cinema'); assert.equal(await title.textContent(), '动感影院播控');
    assert.match(await page.locator('.workspace-registry-breadcrumbs').innerText(), /IO 设备平台/);
    await page.getByRole('navigation', { name: '项目位置' }).getByRole('button', { name: 'IO 设备平台' }).click();
    await search.fill('');
  });
  await check('刷新保留手动收起状态，中文别名和物理路径均可检索', async () => {
    const collapse = page.getByRole('button', { name: '收起IO 设备平台的子项目', exact: true });
    await collapse.click();
    await page.getByRole('button', { name: '刷新工作区', exact: true }).click();
    await page.getByRole('button', { name: '展开IO 设备平台的子项目', exact: true }).waitFor();
    const search = page.getByRole('searchbox', { name: '搜索项目、别名或目录' });
    for (const query of ['动感影院播控', 'O:/synthetic/IODevice/CinemaControl']) {
      await search.fill(query);
      assert.equal(await page.locator('.workspace-registry-node-select:visible').count(), 2);
    }
    await search.fill('');
    await page.getByRole('button', { name: '展开IO 设备平台的子项目', exact: true }).click();
  });
  await check('后代汇总、统一分页与仅此项目切换', async () => {
    await history();
    assert.match(await page.locator('.workspace-session-history-pagination').innerText(), /1–50 \/ 61/);
    await page.getByRole('button', { name: '下一页', exact: true }).click();
    await page.waitForFunction(() => document.querySelector('.workspace-session-history-pagination')?.textContent.includes('51–61'));
    await page.getByRole('combobox', { name: '项目数据范围' }).selectOption('self');
    await page.waitForFunction(() => document.querySelector('.workspace-session-history-pagination')?.textContent.includes('/ 11'));
    await page.getByRole('combobox', { name: '项目数据范围' }).selectOption('descendants');
    await page.waitForFunction(() => document.querySelector('.workspace-session-history-pagination')?.textContent.includes('/ 61'));
  });
  await check('从父级读取子会话并复制或继续时传真实owner', async () => {
    await page.getByRole('button', { name: '预览会话：合成会话 03', exact: true }).click();
    await page.getByRole('button', { name: '复制恢复命令', exact: true }).click();
    await page.waitForFunction(() => window.fixtureCopied === 'synthetic --resume');
    await page.getByRole('button', { name: '继续会话', exact: true }).click();
    assert.equal(await page.evaluate(() => window.hierarchyFixture.calls.findLast(c => c.kind === 'transcript').owner), 'plugins');
    assert.equal(await page.evaluate(() => window.hierarchyFixture.calls.findLast(c => c.kind === 'plan').owner), 'plugins');
    assert.equal(await page.evaluate(() => window.hierarchyFixture.actions.findLast(c => c.sessionId).owner), 'plugins');
    await page.getByRole('button', { name: '关闭预览', exact: true }).click();
  });
  await check('正文搜索继承后代范围和所属中文名', async () => {
    await page.getByRole('button', { name: '搜索正文', exact: true }).click();
    await page.getByRole('searchbox', { name: '搜索当前项目正文' }).fill('needle');
    await page.locator('.workspace-history-inspector').getByRole('button', { name: '搜索', exact: true }).click();
    await page.locator('.workspace-history-inspector-list li').first().waitFor();
    assert.equal(await page.evaluate(() => window.hierarchyFixture.calls.findLast(c => c.kind === 'search').includeChildren), true);
    assert.match(await page.locator('.workspace-history-inspector-list').innerText(), /设备配置与编排/);
    await page.getByRole('button', { name: '关闭正文搜索', exact: true }).click();
  });
  await check('多个中文别名明确选择、原技术名保留', async () => {
    await select('ambiguous'); await page.getByRole('button', { name: '编辑项目', exact: true }).click();
    const dialog = page.getByRole('dialog');
    await dialog.getByLabel('选择展示别名').selectOption('工具乙');
    await dialog.getByRole('button', { name: '保存修改', exact: true }).click();
    await page.waitForFunction(() => document.querySelector('#workspace-selected-project-title')?.textContent === '工具乙');
    assert.equal(await page.evaluate(() => window.hierarchyFixture.saves.at(-1).id), 'ambiguous');
    assert.match(await page.locator('.workspace-registry-technical-name').innerText(), /TechnicalTool/);
  });
  await check('批量整理预览、保存及版本保护恢复入口', async () => {
    await select('root'); await page.getByRole('button', { name: /^项目目录/ }).click();
    await page.getByRole('button', { name: '整理子项目', exact: true }).click();
    const dialog = page.getByRole('dialog');
    await dialog.getByRole('checkbox', { name: 'NewTool' }).check();
    await dialog.getByLabel('子项目别名', { exact: true }).fill('调试工具');
    await dialog.getByRole('button', { name: '预览整理', exact: true }).click();
    await dialog.getByRole('region', { name: '整理预览' }).waitFor();
    assert.equal(await page.evaluate(() => window.hierarchyFixture.calls.filter(c => c.kind === 'organize-preview').length), 1);
    await dialog.getByRole('button', { name: '保存 1 个子项目', exact: true }).click();
    await page.getByRole('button', { name: '恢复本次整理', exact: true }).waitFor();
    await page.getByRole('button', { name: '恢复本次整理', exact: true }).click();
    await page.locator('.confirm-modal').getByRole('button', { name: '恢复整理', exact: true }).click();
    await page.waitForFunction(() => !document.querySelector('.workspace-registry-restore'));
  });
  await check('窄窗层级名称、布局与滚动区域', async () => {
    await select('root'); await history();
    for (const width of [1280, 1024, 768, 320]) {
      await page.setViewportSize({ width, height: 680 });
      await page.screenshot({ path: resolve(evidence, `hierarchy-${width}.png`) });
      const metrics = await page.evaluate(() => ({ overflow: document.documentElement.scrollWidth > innerWidth + 1, list: document.querySelector('.workspace-session-history-list')?.getBoundingClientRect().height, bodyScroll: document.scrollingElement.scrollHeight > innerHeight + 1 }));
      assert.equal(metrics.overflow, false, `${width}: horizontal overflow`);
      assert.equal(metrics.bodyScroll, false, `${width}: page scroll`);
      measurements.push({ width, ...metrics });
      assert(metrics.list > 96, `${width}: history list ${metrics.list}`);
      if (width <= 768) assert.match(await page.getByRole('combobox', { name: '切换项目', exact: true }).locator('option[value=cinema]').innerText(), /IO 设备平台 \/ 动感影院播控/);
    }
  });
  await check('滚轮和键盘只移动当前列表，返回预览保留位置', async () => {
    await page.setViewportSize({ width: 768, height: 680 });
    const list = page.locator('.workspace-session-history-list');
    await list.hover(); await page.mouse.wheel(0, 500);
    await page.waitForFunction(() => document.querySelector('.workspace-session-history-list').scrollTop > 0);
    const before = await list.evaluate(e => e.scrollTop);
    await list.focus(); await page.keyboard.press('PageDown');
    await page.waitForFunction(value => document.querySelector('.workspace-session-history-list').scrollTop > value, before);
    const trigger = page.getByRole('button', { name: /^预览会话/ }).nth(20);
    await trigger.scrollIntoViewIfNeeded();
    const saved = await list.evaluate(e => e.scrollTop);
    await trigger.click();
    const messages = page.locator('.workspace-session-history-messages');
    await messages.waitFor(); await messages.hover(); await page.mouse.wheel(0, 500);
    await page.waitForFunction(() => document.querySelector('.workspace-session-history-messages').scrollTop > 0);
    assert.equal(await page.locator('.main-content').evaluate(e => e.scrollTop), 0);
    await page.getByRole('button', { name: '关闭预览', exact: true }).click();
    assert(Math.abs(await list.evaluate(e => e.scrollTop) - saved) <= 2);
    await list.evaluate(e => { e.scrollTop = e.scrollHeight; });
    await list.hover(); await page.mouse.wheel(0, 500);
    assert.equal(await page.evaluate(() => document.scrollingElement.scrollTop), 0);
    assert.equal(await page.locator('.main-content').evaluate(e => e.scrollTop), 0);
  });
  await check('200%等效视口重排保留项目切换、历史与键盘操作', async () => {
    // A 1280×900 viewport at 200% browser zoom exposes 640×450 CSS pixels.
    await page.setViewportSize({ width: 640, height: 450 });
    await page.screenshot({ path: resolve(evidence, 'hierarchy-zoom-200.png') });
    const metrics = await page.evaluate(() => ({ equivalentZoom: 200, overflow: document.documentElement.scrollWidth > innerWidth + 1, list: document.querySelector('.workspace-session-history-list').getBoundingClientRect().height }));
    measurements.push(metrics);
    assert.equal(metrics.overflow, false);
    assert(metrics.list > 48, `200%: history list ${metrics.list}`);
    await page.getByRole('combobox', { name: '切换项目', exact: true }).selectOption('cinema');
    assert.equal(await title.textContent(), '动感影院播控');
    await page.setViewportSize({ width: 1280, height: 900 });
    await select('root');
  });
  await check('键盘进入子项目弹窗与Esc回还', async () => {
    await page.setViewportSize({ width: 1024, height: 900 });
    const trigger = page.getByRole('button', { name: '新建子项目', exact: true });
    await trigger.focus(); await page.keyboard.press('Enter');
    assert.equal(await page.getByRole('dialog').getByLabel('上级项目', { exact: true }).inputValue(), 'root');
    await page.keyboard.press('Escape');
    assert.equal(await trigger.evaluate(e => e === document.activeElement), true);
  });
  assert.deepEqual(errors, []);
  writeFileSync(resolve(evidence, 'result.json'), JSON.stringify({ passed: results.length, results, measurements, errors }, null, 2));
} catch (error) {
  await page.screenshot({ path: resolve(evidence, 'failure.png') });
  writeFileSync(resolve(evidence, 'failure.json'), JSON.stringify({ results, errors, error: String(error), stack: error.stack }, null, 2));
  throw error;
} finally { await browser.close(); await server.close(); }
