import assert from 'node:assert/strict';
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';
import react from '@vitejs/plugin-react';
const { chromium } = await import(process.env.ONBOARDING_PLAYWRIGHT || 'playwright');

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const evidence = resolve(process.env.ONBOARDING_EVIDENCE_DIR || resolve(root, '../.himind/long-tasks/20261117-simplified-ai-service-onboarding/evidence/browser'));
mkdirSync(evidence, { recursive: true });
const fixture = `
import React from 'react';import{createRoot}from'react-dom/client';
import{AiConnectionsPage}from'/src/pages/AiConnectionsPage';import{ConfirmProvider}from'/src/components/ConfirmDialog';import{agentApi}from'/src/services/agentApi';import'/styles.css';
window.saves=[];window.fetches=[];window.savedFetches=[];window.routeWrites=[];window.invokes=[];window.catalog=['gpt-4.1','gpt-image-1'];window.fetchMode='ready';
const service={id:'existing',display_name:'已存服务',base_url:'https://saved.test/v1',protocol:'openai-chat',model:'shared',models:['shared'],image_model:'',image_adapter:'',enabled_capabilities:['chat'],media:{}};
agentApi.getMediaProviderCatalog=async()=>({providers:[]});
window.checkSaveBridge=input=>{window.__TAURI_INTERNALS__={invoke:async(command,args)=>{window.invokes.push({command,args});return service}};return agentApi.saveAIService(input)};
const noop=async()=>{};
async function fetchModels(input){window.fetches.push(input);if(window.fetchMode==='pending')return new Promise(resolve=>{window.resolveCatalog=resolve});if(window.fetchMode==='error')throw Error('目录接口未提供');return window.catalog}
async function save(input){window.saves.push(input);await new Promise(resolve=>setTimeout(resolve,150));if(window.saveError)throw Error(window.saveError)}
createRoot(document.getElementById('root')).render(<ConfirmProvider><main style={{padding:16}}><AiConnectionsPage initialTab="services" aiServices={{managed:{available:false,reason:'not_authorized'},custom:{services:[service],capability_routes:{chat:[{service_id:'existing',model:''}]},balances:{}},clients:{targets:[]}}} aiServiceTemplates={null} targets={[]} acpProfiles={{profiles:[],local_providers:[]}} gatewayStatus={null} gatewayProfiles={[]} dashboardEnabled={false} identity={null} busyAction={null} testResult={null} onRefresh={noop} onSaveAIService={save} onFetchModels={fetchModels} onFetchSavedModels={async(id,base_url)=>{window.savedFetches.push({id,base_url});return ['shared']}} onFetchBalance={noop} onRemoveAIService={noop} onCloneAIService={async()=>service} onSetCapabilityRoutes={async(...args)=>window.routeWrites.push(args)} onOpenAccount={noop} onApplyTarget={noop} onApplyAll={noop} onRemoveAll={noop} onRemoveTarget={noop} onOpenDirectory={noop} onTest={noop} onImportAIClient={noop} onRemoveAIClient={noop} onSetBindingMode={noop} onRestartGateway={noop} onStopGateway={noop} onSetGatewayPort={noop} onSaveAcpProfile={noop} onSetAcpProfileEnabled={noop} onRemoveAcpProfile={noop}/></main></ConfirmProvider>);
`;
const virtual = resolve(root, '__onboarding-fixture.tsx').replaceAll('\\', '/');
const server = await createServer({ root, configFile: false, cacheDir: resolve(evidence, 'vite-cache'), plugins: [react(), {
  name: 'onboarding-fixture', resolveId(id) { if (id === '/__onboarding-fixture.tsx' || id === virtual) return virtual; }, load(id) { if (id === virtual) return fixture; },
  configureServer(s) { s.middlewares.use('/__onboarding', async (_req, res) => { res.setHeader('Content-Type', 'text/html'); res.end(await s.transformIndexHtml('/__onboarding', '<!doctype html><html lang="zh"><body><script type="module">import RefreshRuntime from "/@react-refresh";RefreshRuntime.injectIntoGlobalHook(window);window.$RefreshReg$=()=>{};window.$RefreshSig$=()=>type=>type;window.__vite_plugin_react_preamble_installed__=true;</script><div id="root"></div><script type="module" src="/__onboarding-fixture.tsx"></script></body></html>')); }); },
}], server: { host: '127.0.0.1', port: 0 } });
let browser, page;
const passed = [], errors = [];
try {
  await server.listen();
  browser = await chromium.launch({ headless: true, ...(process.env.ONBOARDING_CHROMIUM ? { executablePath: process.env.ONBOARDING_CHROMIUM } : {}) });
  page = await browser.newPage({ viewport: { width: 1000, height: 1000 } });
  page.setDefaultTimeout(7000);
  page.on('pageerror', error => errors.push(error.message));
  const url = `http://127.0.0.1:${server.httpServer.address().port}/__onboarding`;
  const dialog = () => page.getByRole('dialog');
  const next = () => dialog().getByRole('button', { name: '选择模型', exact: true }).click();
  const back = () => dialog().getByRole('button', { name: '上一步', exact: true }).click();
  const row = id => page.locator('.ai-model-row').filter({ has: page.getByRole('checkbox', { name: `使用模型 ${id}`, exact: true }) });
  const selected = id => page.getByRole('checkbox', { name: `使用模型 ${id}`, exact: true }).check();
  async function fresh() { await page.goto(url); await page.getByRole('button', { name: '新增 AI 服务', exact: true }).click(); }
  async function custom() { await fresh(); await dialog().getByLabel('Base URL', { exact: true }).fill('https://custom.test/v1'); await dialog().getByLabel('API Key', { exact: true }).fill('fake-test-key'); }
  async function manual(id, operation) { const details = dialog().locator('details').filter({ has: page.locator('summary', { hasText: /^手动添加模型$/ }) }); await details.locator('summary').click(); await details.getByLabel('模型 ID', { exact: true }).fill(id); await details.getByLabel('模型用途', { exact: true }).selectOption(operation); await details.getByRole('button', { name: '添加模型', exact: true }).click(); }
  async function save() { await dialog().getByRole('button', { name: /^(保存服务|保存修改)$/ }).click(); }

  await fresh();
  assert.equal(await page.locator('.ai-service-step').count(), 2);
  await dialog().getByLabel('服务供应商').selectOption('openai');
  assert.equal(await dialog().getByLabel('Base URL', { exact: true }).isVisible(), false);
  await dialog().getByLabel('API Key', { exact: true }).fill('fake-test-key');
  await page.screenshot({ path: resolve(evidence, 'connection.png'), fullPage: true });
  await next(); await row('gpt-image-1').waitFor(); await selected('gpt-image-1');
  await page.screenshot({ path: resolve(evidence, 'models.png'), fullPage: true });
  await save(); await dialog().waitFor({ state: 'hidden' });
  let data = await page.evaluate(() => window.saves.at(-1));
  assert.equal(data.display_name, 'OpenAI API', 'preset name supplies optional service name');
  assert.equal(data.model, 'gpt-4.1'); assert.equal(data.image_model, 'gpt-image-1'); assert.equal(data.image_edit_model, 'gpt-image-1');
  assert.deepEqual(data.enabled_capabilities, ['chat', 'image.generate', 'image.edit', 'vision.describe']);
  assert.deepEqual(await page.evaluate(() => window.routeWrites), []);
  await page.evaluate(() => window.checkSaveBridge(window.saves[0]));
  const bridge = await page.evaluate(() => window.invokes[0]);
  assert.equal(bridge.command, 'save_ai_service');
  assert.deepEqual(bridge.args.enabledCapabilities, data.enabled_capabilities);
  assert.equal(bridge.args.imageModel, 'gpt-image-1');
  assert.equal(bridge.args.visionModel, 'gpt-4.1');
  await page.getByRole('button', { name: '设置默认与备用', exact: true }).click();
  assert.equal(await page.locator('#ai-tab-routes').getAttribute('aria-selected'), 'true');
  passed.push('preset minimal path, mixed model configuration, routing unchanged and optional next step');

  await custom(); await page.evaluate(() => window.catalog = ['mystery', 'image-b']); await next();
  await selected('mystery'); await save(); await page.getByRole('alert').filter({ hasText: '选择用途' }).waitFor();
  assert.equal(await page.evaluate(() => window.saves.length), 0);
  await row('mystery').getByRole('combobox').selectOption('vision.describe');
  await save(); await dialog().waitFor({ state: 'hidden' });
  data = await page.evaluate(() => window.saves[0]);
  assert.equal(data.display_name, 'custom.test', 'custom connection defaults to its hostname');
  assert.equal(data.model, ''); assert.deepEqual(data.models, []); assert.equal(data.vision_model, 'mystery');
  assert.deepEqual(data.enabled_capabilities, ['vision.describe']);
  passed.push('unknown catalog needs explicit use; vision-only payload remains valid with chat disabled');

  await custom(); await page.evaluate(() => window.catalog = ['image-a', 'image-b']); await next();
  for (const id of ['image-a', 'image-b']) { await selected(id); await row(id).getByRole('combobox').selectOption('image.generate'); }
  await dialog().locator('summary', { hasText: /^各用途的服务默认模型$/ }).click();
  await dialog().getByRole('combobox', { name: '生图默认模型', exact: true }).selectOption('image-b');
  await page.evaluate(() => window.saveError = 'fixture save unavailable'); await save();
  await page.getByRole('alert').filter({ hasText: 'fixture save unavailable' }).waitFor();
  assert.equal(await row('image-a').getByRole('checkbox').isChecked(), true);
  await page.evaluate(() => window.saveError = '');
  await dialog().getByRole('button', { name: '保存服务', exact: true }).dblclick();
  await dialog().waitFor({ state: 'hidden' });
  assert.equal(await page.evaluate(() => window.saves.length), 2, 'one failed save + one retry; double click must not duplicate');
  data = await page.evaluate(() => window.saves.at(-1));
  assert.equal(data.image_model, 'image-b'); assert.deepEqual(data.media_models['image.generate'], ['image-a']); assert.deepEqual(data.models, []);
  passed.push('multiple media models and default, save failure retains form, duplicate submission guarded');

  await custom(); await page.evaluate(() => window.fetchMode = 'pending'); await next();
  await page.waitForFunction(() => !!window.resolveCatalog); await back();
  await dialog().getByLabel('Base URL', { exact: true }).fill('https://changed.test/v1');
  await page.evaluate(() => { window.resolveCatalog(['stale-model']); window.fetchMode = 'ready'; window.catalog = ['fresh-model']; });
  await next(); await row('fresh-model').waitFor(); assert.equal(await row('stale-model').count(), 0);
  await selected('fresh-model'); await row('fresh-model').getByRole('combobox').selectOption('audio.speech');
  await back(); await dialog().getByLabel('API Key', { exact: true }).fill('fake-new-key');
  await page.evaluate(() => { window.fetchMode = 'error'; }); await next();
  await page.getByText('目录接口未提供', { exact: false }).waitFor();
  assert.match(await row('fresh-model').innerText(), /先前选择/);
  await page.screenshot({ path: resolve(evidence, 'manual-fallback.png'), fullPage: true });
  await back(); await dialog().getByLabel('接口类型', { exact: true }).selectOption('anthropic');
  await next(); assert.equal(await row('fresh-model').count(), 0);
  await manual('manual-speech', 'audio.speech'); await save(); await dialog().waitFor({ state: 'hidden' });
  data = await page.evaluate(() => window.saves.at(-1)); assert.equal(data.protocol, 'anthropic'); assert.equal(data.speech_model, 'manual-speech');
  passed.push('stale directory cannot overwrite connection; credential change invalidates directory source; protocol clears selection; manual fallback');

  await page.goto(url); await page.getByRole('button', { name: '编辑 AI 服务 已存服务', exact: true }).click();
  await next(); await row('shared').waitFor(); assert.equal(await page.evaluate(() => window.savedFetches.length), 1);
  assert.equal(await row('shared').getByRole('button', { name: '视觉理解 ×', exact: true }).count(), 0);
  await back(); await dialog().getByLabel('Base URL', { exact: true }).fill('https://new-saved.test/v1'); await next();
  await page.getByText('连接已修改，请填写 API Key 后重新获取模型目录。', { exact: true }).waitFor();
  assert.equal(await page.evaluate(() => window.savedFetches.length), 1, 'must not fetch old saved address for changed connection');
  assert.equal(await row('shared').count(), 0); await manual('vision-new', 'vision.describe'); await save(); await dialog().waitFor({ state: 'hidden' });
  data = await page.evaluate(() => window.saves.at(-1)); assert.equal(data.id, 'existing'); assert.equal(data.api_key, ''); assert.equal(data.vision_model, 'vision-new');
  passed.push('edit preserves explicit disabled vision, changed endpoint never queries saved old endpoint');

  await fresh(); await dialog().getByLabel('服务供应商').selectOption('wan-video'); await dialog().getByLabel('API Key', { exact: true }).fill('fake-test-key'); await next();
  await page.getByText('此接口无模型目录。请用预设或手动添加，使用前需验证。', { exact: true }).waitFor(); assert.equal(await page.evaluate(() => window.fetches.length), 0);
  await save(); await dialog().waitFor({ state: 'hidden' }); data = await page.evaluate(() => window.saves[0]); assert.equal(data.video_adapter, 'wan-video'); assert.deepEqual(data.models, []);
  passed.push('provider without generic catalog uses explicit template/manual configuration without inference probe');

  await custom();
  await dialog().locator('summary', { hasText: /^高级连接：第二接入端点$/ }).click();
  await dialog().getByLabel('第二 Base URL', { exact: true }).fill('not-an-api-address');
  await next(); await page.getByRole('alert').filter({ hasText: '第二接入端点不是有效' }).waitFor();
  assert.equal(await page.evaluate(() => window.fetches.length), 0);
  await dialog().getByLabel('第二 Base URL', { exact: true }).fill('https://custom.test/anthropic');
  await dialog().getByLabel('第二接口类型').selectOption('anthropic');
  await page.evaluate(() => window.fetchMode = 'error'); await next(); await manual('custom-image', 'image.generate');
  await dialog().locator('summary', { hasText: /^高级：调用方式与账户用量$/ }).click();
  await dialog().getByLabel('服务 ID', { exact: true }).fill('custom-image-service');
  await dialog().getByLabel('生图调用方式').selectOption('ark-images');
  await dialog().getByLabel('余额/额度查询模板').selectOption('new-api');
  await save(); await page.getByRole('alert').filter({ hasText: 'User ID' }).waitFor();
  await dialog().getByLabel('New API User ID', { exact: true }).fill('23');
  await save(); await dialog().waitFor({ state: 'hidden' });
  data = await page.evaluate(() => window.saves[0]);
  assert.equal(data.id, 'custom-image-service'); assert.equal(data.image_adapter, 'ark-images'); assert.equal(data.balance_user_id, '23');
  assert.deepEqual(data.endpoints, [{ protocol: 'openai-responses', base_url: 'https://custom.test/v1' }, { protocol: 'anthropic', base_url: 'https://custom.test/anthropic' }]);
  passed.push('advanced endpoint validation, custom adapter and account settings remain available');

  await custom(); await page.evaluate(() => window.fetchMode = 'pending'); await next();
  await page.waitForFunction(() => !!window.resolveCatalog); await dialog().getByRole('button', { name: '关闭', exact: true }).click();
  await page.getByRole('button', { name: '新增 AI 服务', exact: true }).click();
  await page.evaluate(() => { window.resolveCatalog(['late-closed-model']); window.fetchMode = 'error'; });
  await dialog().getByLabel('Base URL', { exact: true }).fill('https://reopened.test/v1'); await dialog().getByLabel('API Key', { exact: true }).fill('fake-test-key');
  await next(); await page.getByText('目录接口未提供', { exact: false }).waitFor();
  assert.equal(await row('late-closed-model').count(), 0);
  passed.push('closed dialog rejects a late directory response after reopening');

  for (const width of [390, 880]) {
    await page.setViewportSize({ width, height: 1000 }); await fresh();
    const metrics = await page.evaluate(() => ({ width: innerWidth, body: document.documentElement.scrollWidth, modal: document.querySelector('dialog')?.getBoundingClientRect().width }));
    assert.ok(metrics.body <= width, JSON.stringify(metrics));
    await page.screenshot({ path: resolve(evidence, `connection-${width}.png`), fullPage: true });
  }
  passed.push('normal and narrow viewport rendering');
  assert.deepEqual(errors, []);
  writeFileSync(resolve(evidence, 'result.json'), JSON.stringify({ passed: true, classification: 'Chromium / production React components / mocked host callbacks (not installed-host or provider inference acceptance)', checks: passed, errors, browser: browser.version() }, null, 2));
  console.log(`AI service onboarding browser: ${passed.length} scenario groups passed. Evidence: ${evidence}`);
} catch (error) {
  if (page) { await page.screenshot({ path: resolve(evidence, 'failure.png'), fullPage: true }).catch(() => {}); writeFileSync(resolve(evidence, 'failure.txt'), String(error.stack) + '\n' + await page.locator('body').innerText().catch(() => '')); }
  throw error;
} finally { await browser?.close(); await server.close(); }
