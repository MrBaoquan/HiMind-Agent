import assert from 'node:assert/strict';
import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';
import react from '@vitejs/plugin-react';
import { chromium } from 'playwright';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const evidence = resolve(process.env.WORKFLOW_SUBMISSION_EVIDENCE || resolve(root, '../../.himind/reviews/agent-white-screen-20261123/browser'));
mkdirSync(evidence, { recursive: true });
const server = await createServer({ root, configFile: false, cacheDir: resolve(evidence, 'vite-cache'), plugins: [react(), {
  name: 'workflow-submission-fixture',
  configureServer(vite) {
    vite.middlewares.use('/__workflow-submission', async (_req, res) => {
      res.setHeader('Content-Type', 'text/html; charset=utf-8');
      res.end(await vite.transformIndexHtml('/__workflow-submission', '<!doctype html><html><head><link rel="icon" href="data:,"></head><body><div id="root"></div><script type="module" src="/scripts/fixtures/workflow-submission.tsx"></script></body></html>'));
    });
  },
}], server: { host: '127.0.0.1', port: 0 } });
await server.listen();
let browser;
const errors = [];
const results = [];
try {
  browser = await chromium.launch({ headless: true, channel: 'chrome' });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  page.on('pageerror', error => errors.push(error.message));
  const url = `http://127.0.0.1:${server.httpServer.address().port}/__workflow-submission`;
  for (let round = 0; round < 2; round++) {
    await page.goto(url);
    await page.getByRole('heading', { name: '每日工作日志', exact: true }).waitFor();
    await page.getByRole('button', { name: '模拟提交结果', exact: true }).click();
    await page.getByText('已提交，等待审核', { exact: true }).waitFor();
    assert.equal(await page.getByRole('heading', { name: '每日工作日志', exact: true }).count(), 1);
    results.push(`flat submission rendered after refresh ${round + 1}`);
    await page.getByRole('button', { name: '模拟审核更新', exact: true }).click();
    await page.getByText('待发布', { exact: true }).first().waitFor();
    results.push(`review status update rendered ${round + 1}`);
  }
  await page.screenshot({ path: resolve(evidence, 'workflow-submission.png'), fullPage: true });
  assert.deepEqual(errors, []);
  writeFileSync(resolve(evidence, 'result.json'), JSON.stringify({ results, errors }, null, 2));
  console.log(`PASS ${results.length} workflow submission browser scenarios; no uncaught errors`);
} finally {
  await browser?.close();
  await server.close();
}
