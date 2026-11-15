import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, resolve } from 'node:path';
import { mkdirSync, writeFileSync } from 'node:fs';
import { createServer } from 'vite';
import react from '@vitejs/plugin-react';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const evidence = process.env.SESSION_HISTORY_EVIDENCE || resolve(root, '../target/session-history-browser');
mkdirSync(evidence, { recursive: true });
const require = createRequire(import.meta.url);
const { chromium } = process.env.PLAYWRIGHT_MODULE
  ? await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE).href)
  : require('playwright');

const server = await createServer({
  root,
  cacheDir: resolve(evidence, 'vite-cache'),
  configFile: false,
  plugins: [react(), {
    name: 'session-history-fixture-page',
    configureServer(vite) {
      vite.middlewares.use('/__session-history-fixture', async (request, response, next) => {
        if (new URL(request.url || '/', 'http://fixture.local').pathname !== '/') return next();
        const html = await vite.transformIndexHtml(
          '/__session-history-fixture',
          '<!doctype html><html lang="zh"><head><link rel="icon" href="data:," /></head><body><div id="root"></div><script type="module" src="/scripts/fixtures/workspace-session-history.tsx"></script></body></html>',
        );
        response.setHeader('Content-Type', 'text/html');
        response.end(html);
      });
    },
  }],
  server: { host: '127.0.0.1', port: 0 },
});

await server.listen();
const browser = await chromium.launch({
  headless: true,
  ...(process.env.CHROMIUM_EXECUTABLE ? { executablePath: process.env.CHROMIUM_EXECUTABLE } : {}),
});
const errors = [];

try {
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  await page.addInitScript(() => {
    window.sessionHistoryFixtureReads = [];
    window.addEventListener('session-history-fixture-read', event => {
      window.sessionHistoryFixtureReads.push(event.detail);
    });
    window.historyFixtureActions = [];
    window.addEventListener('history-fixture-action', event => window.historyFixtureActions.push(event.detail));
    Object.defineProperty(navigator, 'clipboard', { value: { writeText: async text => { window.historyFixtureClipboard = text; } } });
  });
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', message => {
    if (message.type() === 'error') errors.push(message.text());
  });
  const failedResponses = [];
  page.on('response', response => {
    if (response.status() >= 400) failedResponses.push(`${response.status()} ${response.url()}`);
  });
  const address = server.httpServer.address();
  await page.goto(`http://127.0.0.1:${address.port}/__session-history-fixture`);

  const rows = page.locator('.workspace-session-history-row');
  const sort = page.getByRole('combobox', { name: '排序会话' });
  await rows.first().waitFor();
  assert.equal(await rows.count(), 50);
  assert.equal(await page.locator('.workspace-session-history-total').textContent(), '55');
  assert.equal(await page.locator('.workspace-session-history-source-state').count(), 5);
  const partialStates = (await page.locator('.workspace-session-history-source-state.partial').allTextContents()).join(' ');
  assert.match(partialStates, /Claude Code.*部分不可用/);
  assert.match(partialStates, /OpenCode.*部分不可用/);
  assert.equal(await page.locator('.workspace-session-history-source').first().textContent(), 'OpenCode');
  assert.equal(await page.getByRole('combobox', { name: '按来源筛选' }).inputValue(), '');
  assert.match(await rows.first().locator('strong').textContent(), /Synthetic session 055/);
  await sort.selectOption('title');
  await page.waitForFunction(() => document.querySelector('.workspace-session-history-row strong')?.textContent?.trim() === 'Synthetic session 001');
  await page.getByRole('button', { name: '下一页' }).click();
  await page.waitForFunction(() => document.querySelector('.workspace-session-history-row strong')?.textContent?.trim() === 'Synthetic session 051');
  await sort.selectOption('oldest');
  await page.waitForFunction(() => document.querySelector('.workspace-session-history-row strong')?.textContent?.trim() === 'Synthetic session 001');
  await sort.selectOption('recent');
  await page.waitForFunction(() => document.querySelector('.workspace-session-history-row strong')?.textContent?.trim() === 'Synthetic session 055');

  await page.getByRole('button', { name: '下一页' }).click();
  await page.waitForFunction(() => document.querySelectorAll('.workspace-session-history-row').length === 5);
  await page.getByRole('button', { name: '上一页' }).click();
  await page.waitForFunction(() => document.querySelectorAll('.workspace-session-history-row').length === 50);

  const sourceFilter = page.getByRole('combobox', { name: '按来源筛选' });
  await sourceFilter.selectOption('claude_code');
  await page.waitForFunction(() => document.querySelector('.workspace-session-history-total')?.textContent === '11');
  assert.equal(await rows.count(), 11);
  assert.equal(await page.locator('.workspace-session-history-source').filter({ hasText: 'Claude Code' }).count(), 11);

  const search = page.getByRole('searchbox', { name: '搜索会话标题或目录' });
  await search.fill('Synthetic session 003');
  await page.waitForFunction(() => document.querySelectorAll('.workspace-session-history-row').length === 1);
  assert.equal(await page.locator('.workspace-session-history-source').first().textContent(), 'Claude Code');
  await search.fill('no-such-session');
  await page.waitForFunction(() => document.querySelectorAll('.workspace-session-history-row').length === 0);
  assert.match(await page.locator('.workspace-session-history-empty').textContent(), /没有匹配的会话/);
  await search.fill('fixture-error');
  await page.getByRole('alert').waitFor();
  assert.match(await page.locator('[role="alert"]').textContent(), /会话列表读取失败/);
  assert.match(await page.locator('.workspace-feedback pre').textContent(), /synthetic_read_failure/);
  await search.fill('');
  await page.waitForFunction(() => document.querySelector('.workspace-session-history-total')?.textContent === '11');

  const expectedLabels = new Map([
    ['dsh', ['HiMind AI', 'Synthetic session 051']],
    ['acp', ['ACP', 'Synthetic session 052']],
    ['claude_code', ['Claude Code', 'Synthetic session 053']],
    ['codex', ['Codex', 'Synthetic session 054']],
    ['opencode', ['OpenCode', 'Synthetic session 055']],
  ]);
  for (const [source, [label, firstTitle]] of expectedLabels) {
    await sourceFilter.selectOption(source);
    await page.waitForFunction(({ expectedLabel, expectedTitle }) => {
      const currentRows = [...document.querySelectorAll('.workspace-session-history-row')];
      return document.querySelector('.workspace-session-history-total')?.textContent === '11'
        && currentRows.length === 11
        && currentRows[0]?.querySelector('strong')?.textContent?.trim() === expectedTitle
        && currentRows.every(row => row.querySelector('.workspace-session-history-source')?.textContent?.trim() === expectedLabel);
    }, { expectedLabel: label, expectedTitle: firstTitle });
    assert.equal(await rows.count(), 11);
    assert.equal(await page.locator('.workspace-session-history-source').filter({ hasText: label }).count(), 11);
    const title = await rows.first().locator('strong').textContent();
    await page.getByRole('button', { name: `预览会话：${title}` }).click();
    await page.waitForFunction(() => document.querySelectorAll('.workspace-session-history-message').length === 50);
    const sequenceLabel = page.locator('.workspace-session-history-preview .workspace-session-history-pagination span');
    if (source === 'claude_code' || source === 'codex') {
      assert.equal(await sequenceLabel.count(), 0);
    } else {
      assert.equal(await sequenceLabel.count(), 1);
      assert.match(await sequenceLabel.textContent(), /序列边界/);
    }
    await page.getByRole('button', { name: '更早消息' }).click();
    await page.waitForFunction(() => document.querySelector('.workspace-session-history-message pre')?.textContent?.includes('Synthetic message 001'));
    assert.equal(await page.locator('.workspace-session-history-message').count(), 50);
    await page.getByRole('button', { name: '更早消息' }).click();
    await page.waitForFunction(() => document.querySelector('.workspace-session-history-message pre')?.textContent?.includes('Synthetic message 000'));
    assert.equal(await page.locator('.workspace-session-history-message').count(), 1);
    assert.equal(await page.getByRole('button', { name: '更早消息' }).isDisabled(), true);
    await page.getByRole('button', { name: '较新消息' }).click();
    await page.waitForFunction(() => document.querySelector('.workspace-session-history-message pre')?.textContent?.includes('Synthetic message 001'));
    await page.getByRole('button', { name: '较新消息' }).click();
    await page.waitForFunction(() => document.querySelector('.workspace-session-history-message pre')?.textContent?.includes('Synthetic message 051'));
    assert.equal(await page.getByRole('button', { name: '较新消息' }).isDisabled(), true);
    assert.deepEqual(await page.evaluate(id => window.sessionHistoryFixtureReads
      .filter(read => read.sessionId === id).map(read => read.cursor), `${source}:synthetic-${firstTitle.slice(-3)}`), [null, '50', '0', '50', null]);
    await page.getByRole('button', { name: '关闭预览' }).click();
    assert.equal(await page.locator('.workspace-session-history-preview').count(), 0);
    assert.equal(await page.locator('.workspace-session-history-message').count(), 0);
  }

  await page.screenshot({ path: resolve(evidence, 'session-history-desktop.png') });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.screenshot({ path: resolve(evidence, 'session-history-mobile.png') });
  const mobileLayout = await page.evaluate(() => ({
    viewportWidth: innerWidth,
    documentWidth: document.documentElement.scrollWidth,
    bodyWidth: document.body.scrollWidth,
    overflowingElements: [...document.querySelectorAll('body *')]
      .map(element => {
        const rect = element.getBoundingClientRect();
        return { element, left: rect.left, right: rect.right, width: rect.width };
      })
      .filter(item => item.right > innerWidth + 1 || item.left < -1)
      .slice(0, 10)
      .map(item => ({
        tag: item.element.tagName,
        className: String(item.element.className).slice(0, 100),
        left: Math.round(item.left),
        right: Math.round(item.right),
        width: Math.round(item.width),
      })),
  }));
  assert.equal(
    mobileLayout.documentWidth <= mobileLayout.viewportWidth + 1
      && mobileLayout.bodyWidth <= mobileLayout.viewportWidth + 1,
    true,
    JSON.stringify(mobileLayout),
  );

  await page.setViewportSize({ width: 1280, height: 900 });
  await page.goto(`http://127.0.0.1:${address.port}/__session-history-fixture?transcript-error-once`);
  await rows.first().waitFor();
  await rows.first().getByRole('button', { name: /^预览会话/ }).click();
  await page.waitForFunction(() => document.querySelector('.workspace-session-history-message pre')?.textContent?.includes('Synthetic message 051'));
  await page.getByRole('button', { name: '更早消息' }).click();
  await page.waitForFunction(() => document.querySelector('.workspace-session-history-message pre')?.textContent?.includes('Synthetic message 001'));
  await page.getByRole('button', { name: '较新消息' }).click();
  await page.getByRole('alert').waitFor();
  assert.match(await page.getByRole('alert').textContent(), /消息读取失败/);
  assert.match(await page.locator('.workspace-feedback pre').textContent(), /synthetic_transcript_read_failure/);
  assert.match(await page.locator('.workspace-session-history-message pre').first().textContent(), /Synthetic message 001/);
  assert.equal(await page.getByRole('button', { name: '较新消息' }).isEnabled(), true);
  await page.getByRole('button', { name: '较新消息' }).click();
  await page.waitForFunction(() => document.querySelector('.workspace-session-history-message pre')?.textContent?.includes('Synthetic message 051'));
  assert.equal(await page.getByRole('alert').count(), 0);
  assert.equal(await page.getByRole('button', { name: '较新消息' }).isDisabled(), true);
  assert.deepEqual(await page.evaluate(() => window.sessionHistoryFixtureReads.map(read => read.cursor)), [null, '50', null, null]);
  await page.getByRole('button', { name: '关闭预览' }).click();
  assert.equal(await page.locator('.workspace-session-history-message').count(), 0);
  // New capabilities use real React components with synthetic providers and launch/clipboard doubles.
  await sourceFilter.selectOption('codex');
  await rows.first().waitFor();
  await page.getByRole('button', { name: '搜索正文', exact: true }).click();
  const bodyQuery = page.getByRole('searchbox', { name: '搜索当前项目正文' });
  await page.waitForFunction(() => document.activeElement?.getAttribute('id') === 'workspace-history-body-query');
  await bodyQuery.fill('needle');
  await page.getByRole('button', { name: '搜索', exact: true }).click();
  await page.getByText(/扫描结束/).waitFor();
  await page.locator('.workspace-history-inspector-list').press('Alt+ArrowDown');
  assert.equal(await page.evaluate(() => document.activeElement?.closest('.workspace-history-inspector-list') != null), true);
  await page.keyboard.press('Enter');
  await page.locator('.workspace-session-history-message.located').waitFor();
  assert.equal(await page.locator('.workspace-session-history-message.located').getAttribute('data-message-id'), '0');
  assert.equal(await page.evaluate(() => document.activeElement?.getAttribute('data-message-id')), '0');
  await page.getByRole('button', { name: '复制消息 0', exact: true }).click();
  assert.match(await page.evaluate(() => window.historyFixtureClipboard), /Synthetic message 000/);
  await page.getByRole('button', { name: '复制恢复命令' }).click();
  await page.getByText('已复制 PowerShell 恢复命令').waitFor();
  assert.match(await page.evaluate(() => window.historyFixtureClipboard), /'resume' 'synthetic-054'/);
  await page.getByRole('button', { name: '继续会话', exact: true }).click();
  await page.getByText('已打开终端；请在原客户端继续操作').waitFor();
  assert.equal(await page.evaluate(() => window.historyFixtureActions.filter(action => action.kind === 'resume').length), 1);
  await page.getByRole('button', { name: '较新消息' }).click();
  await page.waitForFunction(() => document.querySelector('.workspace-session-history-message')?.getAttribute('data-message-id') === '1');
  await page.getByRole('button', { name: '提问导航', exact: true }).click();
  await page.getByText(/扫描结束/).waitFor();
  await page.locator('.workspace-history-inspector-list li > button').first().click();
  await page.locator('.workspace-session-history-message.located').waitFor();
  assert.equal(await page.locator('.workspace-session-history-message.located').getAttribute('data-message-id'), '0');
  await page.getByRole('button', { name: '相关文件', exact: true }).click();
  await page.getByText(/扫描结束/).waitFor();
  await page.locator('.workspace-history-inspector-list li > button').first().click();
  await page.locator('[data-message-id="2"].located').waitFor();
  await page.locator('.workspace-history-file summary').click();
  assert.equal(await page.locator('.workspace-history-diff-before').textContent(), '<script>old()</script>');
  assert.equal(await page.locator('.workspace-history-diff-after').textContent(), 'new()');
  assert.equal(await page.locator('.workspace-history-diff-before script').count(), 0);
  await page.screenshot({ path: resolve(evidence, 'session-history-files.png') });
  await page.getByRole('button', { name: '关闭预览' }).click();
  await page.getByRole('button', { name: '搜索正文', exact: true }).click();
  for (const [query, expected] of [['limited', /结果不完整/], ['partial', /部分来源或会话未读取/], ['no-such-body', /没有匹配的记录/], ['fixture-error', /读取失败，可重新扫描/]]) {
    await bodyQuery.fill(query);
    await page.getByRole('button', { name: '搜索', exact: true }).click();
    await page.getByText(expected).waitFor();
  }
  await bodyQuery.fill('slow');
  await page.getByRole('button', { name: '搜索', exact: true }).click();
  await page.getByRole('button', { name: '取消扫描' }).click();
  await page.getByText(/已取消/).waitFor();
  await page.waitForFunction(() => window.historyFixtureActions.some(action => action.kind === 'cancel'));
  await page.waitForTimeout(900);
  assert.equal(await page.locator('.workspace-history-inspector-list li > button').count(), 0, 'late batch cannot replace cancelled results');
  await page.getByRole('button', { name: '关闭正文搜索' }).click();
  assert.equal(await page.getByRole('button', { name: '搜索正文', exact: true }).evaluate(button => button === document.activeElement), true);

  await page.getByRole('button', { name: '项目相关文件', exact: true }).click();
  await page.getByText(/扫描结束/).waitFor();
  assert.match(await page.locator('.workspace-history-inspector-list').textContent(), /src\/synthetic-file.ts/);
  await page.getByRole('searchbox', { name: '按路径筛选相关文件' }).fill('src/');
  await page.getByRole('button', { name: '搜索', exact: true }).click();
  await page.getByText(/扫描结束/).waitFor();
  assert.equal(await page.evaluate(() => window.historyFixtureActions.filter(action => action.kind === 'start').at(-1).query), 'src/');
  await page.locator('.workspace-history-inspector-list').press('Escape');
  assert.equal(await page.getByRole('button', { name: '项目相关文件', exact: true }).evaluate(button => button === document.activeElement), true);

  for (const viewport of [{ width: 320, height: 640 }, { width: 640, height: 480 }]) {
    await page.setViewportSize(viewport);
    await page.emulateMedia({ colorScheme: 'dark', reducedMotion: 'reduce' });
    await page.getByRole('button', { name: '搜索正文', exact: true }).click();
    await bodyQuery.fill('needle');
    await page.getByRole('button', { name: '搜索', exact: true }).click();
    await page.getByText(/扫描结束/).waitFor();
    await page.locator('.workspace-history-inspector-list li > button').first().click();
    await page.locator('.workspace-session-history-message.located').waitFor();
    const layout = await page.evaluate(() => {
      const list = document.querySelector('.workspace-session-history-messages');
      return { width: document.documentElement.scrollWidth, viewport: innerWidth, bodyHeight: document.documentElement.scrollHeight, height: innerHeight, messageHeight: list.getBoundingClientRect().height };
    });
    assert.ok(layout.width <= layout.viewport + 1 && layout.bodyHeight <= layout.height + 1 && layout.messageHeight >= 80, JSON.stringify(layout));
    await page.screenshot({ path: resolve(evidence, `session-history-capabilities-${viewport.width}.png`) });
    await page.getByRole('button', { name: '关闭预览' }).click();
    await page.getByRole('button', { name: '关闭正文搜索' }).click();
  }
  await page.setViewportSize({ width: 1280, height: 900 });
  // CSS zoom exercises text/control growth; this is not a native WebView zoom claim.
  await page.evaluate(() => {
    // CSS zoom does not shrink dvh like browser zoom. Keep the physical fixture
    // window fixed so this test measures the available reading space honestly.
    document.documentElement.style.zoom = '2';
    document.querySelector('main').style.height = '50dvh';
  });
  await page.getByRole('button', { name: '搜索正文', exact: true }).click();
  await bodyQuery.fill('needle');
  await page.getByRole('button', { name: '搜索', exact: true }).click();
  await page.getByText(/扫描结束/).waitFor();
  await page.locator('.workspace-history-inspector-list li > button').first().click();
  await page.locator('.workspace-session-history-message.located').waitFor();
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth + 1), true);
  assert.ok(await page.locator('.workspace-session-history-messages').evaluate(element => element.clientHeight > 40));
  const zoomBounds = await page.locator('.workspace-session-history-preview .workspace-session-history-pagination').evaluate(element => ({ bottom: element.getBoundingClientRect().bottom, height: innerHeight }));
  assert.ok(zoomBounds.bottom <= zoomBounds.height + 1, JSON.stringify(zoomBounds));
  await page.screenshot({ path: resolve(evidence, 'session-history-css-zoom-200.png') });
  await page.evaluate(() => { document.documentElement.style.zoom = ''; document.querySelector('main').style.height = '100dvh'; });
  await page.goto(`http://127.0.0.1:${address.port}/__session-history-fixture?resume-error`);
  await sourceFilter.selectOption('codex');
  await rows.first().getByRole('button', { name: /^预览会话/ }).click();
  await page.getByRole('button', { name: '继续会话', exact: true }).click();
  await page.getByText('目录归属已变化，请刷新列表。').waitFor();
  await page.goto(`http://127.0.0.1:${address.port}/__session-history-fixture?missing-message`);
  await page.getByRole('button', { name: '搜索正文', exact: true }).click();
  await bodyQuery.fill('needle');
  await page.getByRole('button', { name: '搜索', exact: true }).click();
  await page.getByText(/扫描结束/).waitFor();
  await page.locator('.workspace-history-inspector-list li > button').first().click();
  await page.getByRole('alert').waitFor();
  assert.match(await page.locator('.workspace-feedback pre').textContent(), /来源记录已变化/);
  await page.getByRole('button', { name: '返回结果', exact: true }).click();
  await bodyQuery.waitFor();
  await page.waitForFunction(() => document.activeElement?.getAttribute('id') === 'workspace-history-body-query');
  assert.deepEqual(errors, []);
  assert.deepEqual(failedResponses, []);
  writeFileSync(resolve(evidence, 'session-history-browser-result.json'), JSON.stringify({
    passed: true,
    checks: ['source-labels', 'source-health', 'source-filter', 'sort-change-resets-pagination', 'search-with-filter', 'empty-state', 'error-state', 'server-pagination', 'five-source-preview-pagination', 'native-sequence-boundary', 'transcript-return-refetches-page', 'transcript-error-preserves-navigation-and-retries', 'close-clears-content', 'responsive-overflow', 'body-search-cross-page-jump', 'keyboard-result-navigation', 'message-copy', 'resume-command-copy', 'resume-launch-double', 'located-page-navigation', 'prompt-navigation', 'related-file-source-jump', 'literal-fragment-comparison', 'search-budget-warning', 'search-partial-warning', 'search-empty', 'search-error', 'cancel-and-late-result', 'close-search-restores-focus', 'project-related-files-and-path-filter', 'inspector-escape-and-focus', 'narrow-and-small-height-dark-layout', 'css-zoom-200', 'resume-error', 'missing-source-message-feedback', 'no-console-errors', 'no-http-errors'],
    pageErrors: errors,
    failedResponses,
  }, null, 2));
  console.log('Session history browser integration passed');
} finally {
  await browser.close();
  await server.close();
}
