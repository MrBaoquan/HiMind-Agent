import assert from 'node:assert/strict';
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';
import react from '@vitejs/plugin-react';
import { chromium } from 'playwright';
const agentRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const root = resolve(agentRoot, '../../dashboard-web');
const evidence = resolve(agentRoot, '../../.himind/long-tasks/20261120-extension-review-audience/evidence/browser');
mkdirSync(evidence, { recursive: true });
const fixture = `import React from 'react';import{createRoot}from'react-dom/client';import{App}from'antd';import ReviewWorkbenchPanel from '/src/pages/delivery/ReviewWorkbenchPanel';import'/src/styles.css';createRoot(document.getElementById('root')).render(<App><ReviewWorkbenchPanel token="reviewer-fixture" platformReviewer={false} canManageExtensions={false} formatDateTime={value=>value||''}/></App>);`;
const virtual = resolve(root, '__review-fixture.tsx').replaceAll('\\', '/');
const plugin = {
  name: 'review-fixture',
  resolveId(id) { if (id === virtual || id === '/__review-fixture.tsx') return virtual; },
  load(id) { if (id === virtual) return fixture; },
  configureServer(s) {
    s.middlewares.use('/__review', async (_request, response) => {
      response.setHeader('Content-Type', 'text/html');
      response.end(await s.transformIndexHtml('/__review', '<!doctype html><html lang="zh"><meta name="viewport" content="width=device-width, initial-scale=1"><body><script type="module">import RefreshRuntime from "/@react-refresh";RefreshRuntime.injectIntoGlobalHook(window);window.$RefreshReg$=()=>{};window.$RefreshSig$=()=>type=>type;window.__vite_plugin_react_preamble_installed__=true;</script><div id="root"></div><script type="module" src="/__review-fixture.tsx"></script></body></html>'));
    });
  },
};
const server = await createServer({ root, configFile: false, cacheDir: resolve(evidence, 'review-vite-cache'), define: { 'import.meta.env.VITE_API_BASE': '""' }, plugins: [react(), plugin], server: { host: '127.0.0.1', port: 0 } });
const detail = { kind: 'plugin', review_id: 'review-fixture', product_key: 'com.fixture.plugin', name: '部门共享扩展', description: '供指定研发人员使用', version: '1.0.0', status: 'submitted', submitted_by: '开发者', author: '开发者', artifact_id: 'artifact-fixture', sha256: 'a'.repeat(64), categories: [], permissions: [], capabilities: [], workbench_scope: { department_id: 'engineering', department_name: '研发中心', revision: 7, audience: { mode: 'restricted', user_ids: ['u1'], department_ids: ['engineering'] }, audience_users: ['张三'], audience_departments: ['研发中心'] } };
let browser;
const calls = [];
const decisions = [];
const errors = [];
let page;
try {
  await server.listen();
  browser = await chromium.launch({ headless: true, ...(process.env.AUDIENCE_CHROMIUM ? { executablePath: process.env.AUDIENCE_CHROMIUM } : {}) });
  page = await browser.newPage({ viewport: { width: 1200, height: 900 } });
  page.setDefaultTimeout(25000);
  page.on('pageerror', error => {errors.push(error.message);console.error('PAGE',error.message)});
  await page.route('**/api/**', async route => {
    const req = route.request();
    const path = new URL(req.url()).pathname;
    calls.push(path);
    let body;
    if (path === '/api/distribution/review-queue') body = { items: decisions.length ? [] : [{ key: 'plugin:fixture', kind: 'plugin', review_id: detail.review_id, product_key: detail.product_key, name: detail.name, author: detail.author, risk: 'optional' }], total: decisions.length ? 0 : 1, page: 1, page_size: 20 };
    else if (path.endsWith('/decision')) { decisions.push(req.postDataJSON()); body = { ...detail, status: 'approved' }; }
    else if (path === '/api/distribution/reviews/plugin/review-fixture') body = detail;
    else if (path.endsWith('/review-history')) body = { items: [] };
    else throw Error(`unexpected request ${path}`);
    await route.fulfill({ contentType: 'application/json', body: JSON.stringify(body) });
  });
  await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__review`, { waitUntil: 'domcontentloaded', timeout: 30000 });
  await page.getByRole('button', { name: /查看提交/ }).click();
  const drawer = page.getByRole('dialog');
  await drawer.getByText('人员 · 张三', { exact: true }).waitFor();
  await drawer.getByText('部门 · 研发中心（含下级）', { exact: true }).waitFor();
  assert.equal(await page.getByRole('tab', { name: '部门审核人' }).count(), 0);
  assert.equal(await page.getByRole('tab', { name: '发布范围' }).count(), 0);
  await page.screenshot({ path: resolve(evidence, 'department-review.png'), fullPage: true });
  await drawer.getByRole('button', { name: /通过并上架/ }).click();
  await page.locator('.ant-modal').getByRole('button', { name: /通过并上架/ }).click();
  await page.getByText('已通过审核并上架', { exact: true }).waitFor();
  assert.equal(decisions.length, 1);
  assert.equal(decisions[0].scope_revision, 7);
  assert.equal(decisions[0].artifact_id, 'artifact-fixture');
  assert.equal(calls.some(path => path.includes('settings') || path.includes('inbox') || path.includes('reviewers')), false);
  assert.deepEqual(errors, []);
  const report = { passed: ['部门审核人仅展示审核入口', '显示人员和部门受众', '审批绑定受众修订和制品', '不请求平台配置或全局待办'], pageErrors: errors };
  writeFileSync(resolve(evidence, 'review-report.json'), JSON.stringify(report, null, 2));
  console.log(JSON.stringify(report));
} catch (error) {
  console.error('CALLS', JSON.stringify(calls), 'ERRORS', JSON.stringify(errors));
  if (page) { console.error((await page.locator('body').innerText()).slice(0,3000)); await page.screenshot({path:resolve(evidence,'review-debug.png'),fullPage:true}); }
  throw error;
} finally {
  await browser?.close();
  await server.close();
}
