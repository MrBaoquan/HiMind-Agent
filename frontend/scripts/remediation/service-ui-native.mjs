// Preparation-only native AT session. It is separate from the six frozen checks.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync, existsSync, rmSync } from 'node:fs';
import { resolve } from 'node:path';
import { createRuntime, workspace } from './service-ui-runtime.mjs';
import { createServer } from 'vite';
import react from '@vitejs/plugin-react';
import { chromium } from 'playwright';

const runtime = createRuntime('native');
const directory = runtime.directory;
const root = resolve(workspace, 'frontend');
const source = readFileSync(resolve(root, 'scripts/remediation/service-ui-browser.mjs'), 'utf8');
const fixture = source.match(/const fixture = `([\s\S]*?)`;/)?.[1];
assert.ok(fixture, 'Native session must use the original service fixture');
const html = source.match(/server\.transformIndexHtml\('\/__service-ui','([^']+)'\)/)?.[1];
assert.ok(html?.includes('__vite_plugin_react_preamble_installed__'), 'Original entry Refresh preamble required');
const virtual = resolve(root, '__service-ui-fixture.tsx').replaceAll('\\', '/');
const save = (name, value) => writeFileSync(resolve(directory, name), JSON.stringify(value, null, 2));
let context, server, interval, deadline, closing = false;
async function close(reason) {
  if (closing) return;
  closing = true;
  clearInterval(interval);
  clearTimeout(deadline);
  try { await context?.close(); } finally {
    await server?.close();
    rmSync(resolve(directory, 'p'), { recursive: true, force: true, maxRetries: 3, retryDelay: 100 });
    save('cleanup.json', { reason, browserClosed: !context?.browser()?.isConnected(), serverClosed: !server?.httpServer?.listening });
  }
}
process.once('SIGINT', () => { void close('SIGINT'); });
process.once('SIGTERM', () => { void close('SIGTERM'); });
try {
  server = await createServer({ root, configFile: false, cacheDir: runtime.cache,
    plugins: [react(), { name: 'service-native-at',
      resolveId(id) { if (id === '/__service-ui-fixture.tsx' || id === virtual) return virtual; },
      load(id) { if (id === virtual) return fixture; },
      configureServer(instance) { instance.middlewares.use('/__service-ui', async (request, response, next) => {
        if (request.url && !request.url.startsWith('/?') && request.url !== '/') return next();
        response.setHeader('Content-Type', 'text/html');
        response.end(await instance.transformIndexHtml('/__service-ui', html.replace('<html lang="zh">', '<html lang="zh"><head><title>Service UI native AT</title></head>')));
      }); },
    }], server: { host: '127.0.0.1', port: 0 } });
  await server.listen();
  mkdirSync(resolve(directory, 'p'));
  mkdirSync(resolve(directory, 'a'));
  context = await chromium.launchPersistentContext(resolve(directory, 'p'), {
    artifactsDir: resolve(directory, 'a'), channel: 'chrome', headless: false,
    viewport: null, args: ['--window-size=1200,900', '--force-renderer-accessibility'], timeout: 30000,
  });
  const page = context.pages()[0];
  const url = `http://127.0.0.1:${server.httpServer.address().port}/__service-ui/`;
  const errors = [];
  page.on('pageerror', error => { errors.push(error.message); save('page-errors.json', errors); });
  await page.goto(url);
  await page.getByRole('button', { name: '新增 AI 服务', exact: true }).waitFor();
  const info = { directory, url, browser: context.browser().version(), api: 'original isolated fictitious fixture',
    source_sha256: createHash('sha256').update(source).digest('hex'), started_at: new Date().toISOString(),
    native_zoom: 'must be changed and observed through browser chrome; CSS/CDP emulation is not native zoom',
    screen_reader: 'external portable NVDA; no speech claim from AX or DOM' };
  save('session.json', info);
  writeFileSync(resolve(process.env.SERVICE_UI_RUNTIME_ROOT, 'native-session.json'), JSON.stringify(info, null, 2));
  let busy = false;
  interval = setInterval(async () => {
    const requestPath = resolve(directory, 'request.json');
    if (busy || !existsSync(requestPath)) return;
    busy = true;
    let request;
    try {
      request = JSON.parse(readFileSync(requestPath, 'utf8'));
      rmSync(requestPath);
      assert.match(request.id, /^[a-z0-9-]+$/);
      if (request.action === 'stop') { await close('requested'); return; }
      if (request.action === 'catalog') {
        assert.ok(['ready', 'error', 'denied', 'partial'].includes(request.mode));
        await page.evaluate(mode => { window.setCatalogMode(mode); window.refresh(); }, request.mode);
        await page.waitForTimeout(400);
      } else assert.equal(request.action, 'snapshot');
      await page.screenshot({ path: resolve(directory, request.id + '.png'), fullPage: true });
      save(request.id + '.json', await page.evaluate(() => ({
        time: new Date().toISOString(), innerWidth, innerHeight, devicePixelRatio, outerWidth,
        cssZoom: getComputedStyle(document.body).zoom, scrollWidth: document.documentElement.scrollWidth,
        focus: { id: document.activeElement.id, text: document.activeElement.textContent?.slice(0, 150), tag: document.activeElement.tagName },
        modals: [...document.querySelectorAll('dialog:modal')].map(element => ({ title: element.getAttribute('aria-labelledby'), containsFocus: element.contains(document.activeElement), text: element.innerText })),
        live: [...document.querySelectorAll('[role="alert"],[role="status"]')].map(element => ({ role: element.getAttribute('role'), text: element.textContent })),
      })));
    } catch (error) { save((request?.id || 'request') + '-failure.json', { error: String(error.stack) }); process.exitCode = 1; }
    finally { busy = false; }
  }, 200);
  deadline = setTimeout(() => { process.exitCode = 1; void close('deadline'); }, 1200000);
  console.log('Native AT session ready: ' + directory);
} catch (error) { save('failure.json', { error: String(error.stack) }); await close('error'); throw error; }
