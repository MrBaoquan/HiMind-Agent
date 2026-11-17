import assert from 'node:assert/strict';
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';
import react from '@vitejs/plugin-react';
import { chromium } from 'playwright';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const evidence = resolve(root, '../.himind/long-tasks/20261117-ai-client-source-lifecycle/evidence/browser');
mkdirSync(evidence, { recursive: true });
const fixture = `
import React from 'react';import{createRoot}from'react-dom/client';
import{AiToolsPanel}from'/src/pages/AiToolsPanel';import{ConfirmProvider}from'/src/components/ConfirmDialog';import'/styles.css';
const mode=new URLSearchParams(location.search).get('case')||'codex';
window.calls=[];window.refreshes=0;window.failApply=false;window.conflict=false;
window.__TAURI_INTERNALS__={invoke:async(command,args)=>{
 window.calls.push({command,args});
 if(command==='plan_ai_client_source')return{...args.input,revision:'fixture-current-revision',config_path:'isolated/client-config',scope:'config_directory',changes:['保留 MCP 配置','停止来源自动同步'],warnings:window.conflict?['检测到外部修改']:[],conflict:window.conflict,detail:'这是配置预览，尚未切换',load_hint:'重启客户端后核对来源'};
 if(command==='apply_ai_client_source'){if(window.failApply)throw Error('来源切换计划已过期，请重新预览');return{ok:true,effective_status:'not_verified',auth_status:'unknown',detail:'配置已写入',load_hint:'重启客户端后核对来源',warnings:[]}}
 throw Error('Unexpected host command '+command);
}};
const isOpen=mode==='opencode',isDesktop=mode==='desktop';
const target=isOpen?'opencode':isDesktop?'claude-desktop':'codex';
const status={target,state:isOpen?'imported':'not_imported',client_detected:true,detail:'fixture',models:isOpen?['service/model','second']:[],service:isOpen?'custom:fixture':'',source:{capabilities:{registration_effect:isOpen?'additive':isDesktop?'configuration_profile':'changes_default',native_account:!isOpen&&!isDesktop,select_default:isOpen,selection_hint:isOpen?'添加来源不改变默认模型；用 /models 选择':'原生账号由客户端登录',load_hint:'重启客户端后核对来源'},registered:isOpen,managed:isOpen,configured_source:isOpen?'other_provider':'native_provider',default_model:isOpen?'openai/previous':'',auth_status:'unknown',effective_status:'not_verified',scope:'config_directory',config_path:'isolated/client-config',can_restore:isOpen,warnings:[]}};
const noop=async()=>{};
const service={id:'fixture',display_name:'Fixture API',base_url:'https://fixture.invalid/v1',protocol:'openai-responses',model:'service/model',models:['service/model'],enabled_capabilities:['chat']};
createRoot(document.getElementById('root')).render(<ConfirmProvider><main style={{padding:20}}><AiToolsPanel targets={[]} aiServices={{managed:{available:false},custom:{services:isOpen?[service]:[],capability_routes:{},balances:{}},clients:{targets:[status]}}} acpProfiles={{profiles:[],local_providers:[]}} gatewayStatus={null} gatewayProfiles={[]} dashboardEnabled={false} identity={null} busyAction={null} testResult={null} onRefresh={()=>window.refreshes++} onApplyTarget={noop} onApplyAll={noop} onRemoveAll={noop} onRemoveTarget={noop} onOpenDirectory={noop} onTest={noop} onOpenAccount={noop} onImportAIClient={noop} onRemoveAIClient={noop} onOpenModelSources={noop} onSetBindingMode={noop} onRestartGateway={noop} onStopGateway={noop} onSetGatewayPort={noop} onSaveAcpProfile={noop} onSetAcpProfileEnabled={noop} onRemoveAcpProfile={noop}/></main></ConfirmProvider>);
`;
const virtual = resolve(root, '__source-fixture.tsx').replaceAll('\\', '/');
const server = await createServer({ root, configFile: false, cacheDir: resolve(evidence, 'vite-cache'), plugins: [react(), {
  name: 'client-source-fixture',
  resolveId(id) { if (id === '/__source-fixture.tsx' || id === virtual) return virtual; },
  load(id) { if (id === virtual) return fixture; },
  configureServer(s) { s.middlewares.use('/__source', async (_req, res) => { res.setHeader('Content-Type', 'text/html'); res.end(await s.transformIndexHtml('/__source', '<!doctype html><html lang="zh"><body><script type="module">import RefreshRuntime from "/@react-refresh";RefreshRuntime.injectIntoGlobalHook(window);window.$RefreshReg$=()=>{};window.$RefreshSig$=()=>type=>type;window.__vite_plugin_react_preamble_installed__=true;</script><div id="root"></div><script type="module" src="/__source-fixture.tsx"></script></body></html>')); }); },
}], server: { host: '127.0.0.1', port: 0 } });
let browser;
const passed = [], errors = [];
try {
  await server.listen(); browser = await chromium.launch({ headless: true, ...(process.env.HIMIND_SOURCE_CHROMIUM ? { executablePath: process.env.HIMIND_SOURCE_CHROMIUM } : {}) });
  const page = await browser.newPage({ viewport: { width: 1100, height: 950 } });
  page.setDefaultTimeout(8000); page.on('pageerror', error => errors.push(error.message));
  const url = `http://127.0.0.1:${server.httpServer.address().port}/__source`;
  const dialog = () => page.getByRole('dialog');
  async function fresh(mode = 'codex') { await page.goto(`${url}?case=${mode}`); await page.getByRole('button', { name: '管理', exact: true }).click(); }
  async function hostCalls() { return page.evaluate(() => window.calls); }

  await fresh();
  assert.equal(await page.getByRole('button', { name: '使用 ChatGPT 账号', exact: true }).isVisible(), true);
  await page.getByRole('button', { name: '使用 ChatGPT 账号', exact: true }).click();
  await dialog().waitFor(); assert.equal((await hostCalls()).length, 1);
  assert.match(await dialog().innerText(), /尚未切换/);
  await dialog().getByRole('button', { name: '取消', exact: true }).click();
  assert.equal((await hostCalls()).length, 1);
  passed.push('native account remains available with no Agent service; cancelling plan performs no apply');

  await page.getByRole('button', { name: '使用 ChatGPT 账号', exact: true }).click();
  await dialog().getByRole('button', { name: '应用配置', exact: true }).click();
  await page.getByText('配置已写入，客户端生效待核对。重启客户端后核对来源', { exact: true }).waitFor();
  let calls = await hostCalls();
  assert.equal(calls.at(-1).command, 'apply_ai_client_source');
  assert.equal(calls.at(-1).args.input.expected_revision, 'fixture-current-revision');
  assert.equal(calls.at(-1).args.input.accept_drift, false);
  assert.equal(await page.evaluate(() => window.refreshes), 1);
  await page.screenshot({ path: resolve(evidence, 'codex-native.png'), fullPage: true });
  passed.push('UI plan/apply bridges use revision and display unverified client activation');

  await fresh(); await page.evaluate(() => window.conflict = true);
  await page.getByRole('button', { name: '使用 ChatGPT 账号', exact: true }).click();
  await dialog().getByText('检测到外部修改', { exact: false }).waitFor();
  await dialog().getByRole('button', { name: '确认变更并覆盖', exact: true }).click();
  await page.getByText('配置已写入，客户端生效待核对。重启客户端后核对来源', { exact: true }).waitFor();
  assert.equal((await hostCalls()).at(-1).args.input.accept_drift, true);
  passed.push('external drift is previewed and only explicit confirmation sets overwrite consent');

  await fresh(); await page.evaluate(() => window.failApply = true);
  await page.getByRole('button', { name: '使用 ChatGPT 账号', exact: true }).click();
  await dialog().getByRole('button', { name: '应用配置', exact: true }).click();
  await page.getByRole('alert').filter({ hasText: '计划已过期' }).waitFor();
  assert.equal(await page.evaluate(() => window.refreshes), 0);
  assert.equal(await page.getByRole('button', { name: '使用 ChatGPT 账号', exact: true }).isEnabled(), true);
  passed.push('stale plan error remains visible and enables a fresh retry without false success');

  await fresh('opencode');
  assert.equal(await page.getByRole('button', { name: '使用 ChatGPT 账号', exact: true }).count(), 0);
  await page.getByRole('combobox', { name: 'opencode 默认模型', exact: true }).selectOption('service/model');
  await page.getByRole('button', { name: '设为默认模型', exact: true }).click();
  assert.equal((await hostCalls())[0].args.input.model, 'service/model');
  await dialog().getByRole('button', { name: '应用配置', exact: true }).click();
  await page.getByText('配置已写入，客户端生效待核对。重启客户端后核对来源', { exact: true }).waitFor();
  assert.equal((await hostCalls()).at(-1).args.input.action, 'set_default');
  await page.getByRole('button', { name: '恢复分发前配置', exact: true }).click();
  assert.equal((await hostCalls()).at(-1).args.input.action, 'restore_previous');
  await dialog().getByRole('button', { name: '取消', exact: true }).click();
  await page.screenshot({ path: resolve(evidence, 'opencode-default.png'), fullPage: true });
  passed.push('additive provider offers explicit exact default-model selection and distinct history restoration');

  await fresh();
  await page.getByRole('button', { name: '使用 ChatGPT 账号', exact: true }).dblclick();
  await dialog().waitFor();
  assert.equal((await hostCalls()).length, 1);
  await dialog().getByRole('button', { name: '取消', exact: true }).click();
  passed.push('double click creates one pending plan and no duplicate submission');

  await page.setViewportSize({ width: 720, height: 1000 });
  await fresh('opencode');
  const sourceLayout = await page.locator('.ai-client-source').evaluate(element => ({ width: element.clientWidth, scroll: element.scrollWidth }));
  assert.ok(sourceLayout.scroll <= sourceLayout.width + 1, 'source controls wrap inside the available panel');
  passed.push('source controls fit ordinary narrow window width');

  await fresh('desktop');
  assert.equal(await page.getByRole('button', { name: '使用原生账号', exact: true }).count(), 0);
  assert.equal(await page.getByRole('button', { name: '使用 ChatGPT 账号', exact: true }).count(), 0);
  passed.push('unsupported surface exposes no native-account switch');
  assert.deepEqual(errors, []);
  writeFileSync(resolve(evidence, 'result.json'), JSON.stringify({ passed, errors, provenance: 'Actual React tools panel; mocked Tauri capability responses, no production account changes' }, null, 2));
  console.log(`${passed.length} client-source browser scenarios passed`);
} finally { await browser?.close(); await server.close(); }
