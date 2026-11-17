import assert from 'node:assert/strict';
import { chromium } from 'playwright';
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const evidence = resolve('../.himind/long-tasks/20261117-sustainable-collaboration-workbench/evidence/prototype');
mkdirSync(evidence, { recursive: true });
let browser;
try { browser = await chromium.launch({ headless: true }); }
catch { browser = await chromium.launch({ headless: true, channel: 'msedge' }); }
const errors = [];
try {
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  page.on('pageerror', error => errors.push(error.message));
  await page.goto(pathToFileURL(resolve('../docs/prototypes/collaboration-workbench.html')).href);
  assert.match(await page.locator('nav').innerText(), /示例数据/);
  assert.equal(await page.locator('#concurrency').innerText(), '2');
  for (const scene of ['running', 'waiting', 'confirmation', 'verification', 'completed', 'failed', 'disconnected']) {
    await page.locator('#scenario').selectOption(scene);
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), true);
    if (scene === 'completed') assert.equal(await page.locator('#deliver').getAttribute('aria-selected'), 'true');
    if (scene === 'disconnected') {
      assert.equal(await page.locator('#stale').isVisible(), true);
      assert.equal(await page.locator('#concurrency').innerText(), '—');
    }
    await page.screenshot({ path: resolve(evidence, `${scene}.png`) });
  }
  await page.locator('#scenario').selectOption('running');
  await page.locator('#chat').click();
  await page.locator('#input').fill('为什么测试还没开始');
  await page.locator('#send').click();
  assert.equal(await page.locator('#scenario').inputValue(), 'running');
  assert.match(await page.locator('#messages').innerText(), /询问没有修改计划/);
  await page.locator('#input').fill('增加离线能力，界面继续');
  await page.locator('#send').click();
  assert.equal(await page.locator('#change').isVisible(), true);
  await page.locator('#apply').click();
  assert.equal(await page.locator('#change').isVisible(), false);
  assert.match(await page.locator('#dataTitle').innerText(), /离线/);
  assert.deepEqual(errors, []);
  writeFileSync(resolve(evidence, 'result.json'), JSON.stringify({ kind: 'prototype_only', scenarios: 7, interactions: ['query_read_only','instruction_diff','confirm_change'], result: 'passed', errors }, null, 2));
  console.log('Prototype: 7 states and query/change/confirmation interactions passed (example data only).');
} finally { await browser.close(); }
