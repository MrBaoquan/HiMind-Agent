import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { createRequire } from 'node:module';
const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(resolve(here, '../../frontend/package.json'));
const { chromium } = process.env.PLAYWRIGHT_MODULE ? await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE).href) : require('playwright');
const evidence = resolve(here, '../../.himind/long-tasks/20261116-plugin-ai-service-selection/evidence');
await mkdir(evidence, { recursive: true });
const server = createServer(async (request, response) => {
  const name = request.url.endsWith('client.mjs') ? 'client.mjs'
    : request.url.endsWith('service-selector.mjs') ? 'service-selector.mjs' : 'example.html';
  response.setHeader('Content-Type', name.endsWith('.mjs') ? 'text/javascript' : 'text/html');
  response.end(await readFile(resolve(here, name)));
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const browser = await chromium.launch({ headless: true, ...(process.env.CHROMIUM_EXECUTABLE ? { executablePath: process.env.CHROMIUM_EXECUTABLE } : {}) });
const page = await browser.newPage({ viewport: { width: 1200, height: 1200 } });
const errors = [];
page.on('pageerror', error => errors.push(error.message));
try {
  await page.addInitScript(() => {
    window.calls = [];
    const binding = cap => ({ capability: cap, adapter: cap === 'image.generate' ? 'openai-images' : 'openai-speech',
      model: 'default', models: ['default', 'alternative', 'slow'], execution_mode: 'sync', eligible: true,
      voice_catalog: 'unsupported', features: [], health: { state: 'unknown' } });
    window.__TAURI_INTERNALS__ = { invoke: async (command, { capabilityId, input }) => {
      window.calls.push({ command, capability: capabilityId, input });
      if (command !== 'invoke_plugin_view_capability') throw new Error('Unexpected privileged command');
      if (capabilityId === 'media.provider.catalog') {
        if (window.failCatalog) throw new Error('Agent 暂不可用');
        return { providers: ['a', 'b'].map(id => ({ id, display_name: id === 'a' ? '图片语音服务 A' : '图片语音服务 B', media_capabilities: [binding('image.generate'), binding('audio.speech')] })) };
      }
      if (capabilityId === 'media.route.plan') {
        if (input.selection.model === 'slow') await new Promise(resolve => setTimeout(resolve, 120));
        const service = input.selection.service_id || 'a';
        const model = input.selection.model || 'default';
        const eligible = ['a', 'b'].includes(service) && ['default', 'alternative', 'slow'].includes(model);
        return { state: eligible ? 'resolved' : 'missing', reason: eligible ? 'configured' : 'model_unavailable',
          billable: false, selected: { eligible, service_id: service, display_name: '服务 ' + service.toUpperCase(), model } };
      }
      if (['media.image.generate', 'media.audio.speech'].includes(capabilityId)) return { actual: { service_id: input.selection.service_id || 'a', model: input.selection.model || 'default' }, artifacts: [{ path: 'fixture-output' }], usage: { state: 'unknown' }, fallback_used: false };
      throw new Error('Unexpected capability');
    } };
  });
  const address = server.address();
  const url = `http://127.0.0.1:${address.port}/com.example/main/ui/index.html`;
  await page.goto(url);
  const image = page.locator('section').nth(0);
  const audio = page.locator('section').nth(1);
  await image.getByRole('button', { name: '生成图片', exact: true }).waitFor();
  await page.waitForFunction(() => !document.querySelector('#image-run').disabled);
  assert.match(await image.getByRole('status').textContent(), /服务 A \/ default/);
  assert.equal(await image.getByRole('combobox', { name: '模型' }).isDisabled(), true);
  await image.getByRole('combobox', { name: '服务', exact: true }).selectOption('b');
  await image.getByRole('combobox', { name: '模型' }).selectOption('alternative');
  await page.waitForFunction(() => !document.querySelector('#image-run').disabled);
  assert.equal(await image.getByRole('checkbox').isChecked(), false);
  await image.getByRole('button', { name: '记住本项目选择' }).click();
  await image.getByRole('button', { name: '生成图片', exact: true }).click();
  const request = await page.evaluate(() => window.calls.find(call => call.capability === 'media.image.generate'));
  assert.deepEqual(request.input.selection, { mode: 'pinned', service_id: 'b', model: 'alternative' });
  assert.equal(request.input.fallback, 'none');
  await page.reload();
  await page.waitForFunction(() => !document.querySelector('#image-run').disabled);
  assert.equal(await image.getByRole('combobox', { name: '服务', exact: true }).inputValue(), 'b');
  assert.equal(await image.getByRole('combobox', { name: '模型' }).inputValue(), 'alternative');
  assert.equal(await audio.getByRole('combobox', { name: '服务', exact: true }).inputValue(), '');
  await image.getByRole('combobox', { name: '模型' }).selectOption('slow');
  await image.getByRole('combobox', { name: '模型' }).selectOption('alternative');
  await page.waitForFunction(() => !document.querySelector('#image-run').disabled);
  await page.waitForTimeout(180);
  assert.match(await image.getByRole('status').textContent(), /alternative/);
  await audio.getByRole('combobox', { name: '服务', exact: true }).selectOption('b');
  await audio.getByRole('combobox', { name: '模型' }).selectOption('alternative');
  await page.waitForFunction(() => !document.querySelector('#audio-run').disabled);
  await audio.getByRole('button', { name: '使用服务默认音色生成' }).click();
  const audioRequest = await page.evaluate(() => window.calls.find(call => call.capability === 'media.audio.speech'));
  assert.deepEqual(audioRequest.input.selection, { mode: 'pinned', service_id: 'b', model: 'alternative' });
  assert.equal(audioRequest.input.voice, undefined);
  // Saved stale selection stays visible and blocked; no automatic reset to the default model.
  await page.evaluate(() => {
    const key = JSON.stringify(['himind.ai.selection', 1, 'com.example', 'example-project', 'image.generate']);
    localStorage.setItem(key, JSON.stringify({ selection: { mode: 'pinned', service_id: 'b', model: 'retired' } }));
  });
  await page.reload();
  await image.getByText(/当前选择不可执行/).waitFor();
  assert.equal(await image.getByRole('button', { name: '生成图片', exact: true }).isDisabled(), true);
  assert.equal(await image.getByRole('combobox', { name: '模型' }).inputValue(), 'retired');
  await page.screenshot({ path: resolve(evidence, 'plugin-selector-stale.png'), fullPage: true });
  await image.getByRole('combobox', { name: '服务', exact: true }).selectOption('');
  await page.waitForFunction(() => !document.querySelector('#image-run').disabled);
  await page.evaluate(() => { window.failCatalog = true; });
  await image.getByRole('button', { name: '刷新可用服务' }).click();
  await image.getByText(/无法读取服务/).waitFor();
  assert.equal(await image.getByRole('button', { name: '生成图片', exact: true }).isDisabled(), true);
  await page.evaluate(() => { window.failCatalog = false; });
  await image.getByRole('button', { name: '刷新可用服务' }).click();
  await page.waitForFunction(() => !document.querySelector('#image-run').disabled);
  const calls = await page.evaluate(() => window.calls);
  assert.ok(calls.every(call => !call.capability.startsWith('ai.service.')));
  assert.deepEqual(errors, []);
  await page.screenshot({ path: resolve(evidence, 'plugin-selector-ready.png'), fullPage: true });
  await writeFile(resolve(evidence, 'plugin-selector-browser.json'), JSON.stringify({ passed: true, checks: ['image/audio exact selection', 'project preference reload', 'capability isolation', 'stale selection visible/blocked', 'late plan response ignored', 'Agent outage/refresh recovery', 'no global route mutation'], calls }, null, 2));
  console.log('Plugin selector browser: 7 scenarios passed (fixture host transport; real shared selector/client).');
} finally {
  await browser.close();
  await new Promise(resolve => server.close(resolve));
}
