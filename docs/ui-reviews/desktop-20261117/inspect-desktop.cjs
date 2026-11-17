const { chromium } = require('../../../target/session-history-playwright/node_modules/playwright');
const fs = require('node:fs/promises');
const path = require('node:path');

(async () => {
  const browser = await chromium.connectOverCDP('http://127.0.0.1:9311');
  try {
    const pages = browser.contexts().flatMap(context => context.pages());
    const page = pages.find(candidate => candidate.url() === 'http://tauri.localhost/');
    if (!page) throw new Error('Running Agent main WebView was not found');
    const results = [];
    const targets = [
      ['01-home', '首页'], ['02-tasks', '我的任务'], ['03-inbox', '待处理'],
      ['04-workspaces', '工作区'], ['05-workflows', '工作流'],
      ['06-schedules', '定时计划'], ['07-market', '市场'],
      ['08-installed', '我的能力'], ['09-development', '开发工作区'],
    ];
    for (const [id, label] of targets) {
      const modal = page.locator('[role="dialog"]:visible');
      if (await modal.count()) throw new Error('A dialog is open; stop without dismissing unknown work');
      await page.getByRole('navigation', { name: '主导航' }).getByRole('button', { name: label, exact: true }).click();
      const heading = label === '开发工作区' ? /扩展开发|开发工作区/ : label === '待处理' ? /需要我处理/ : new RegExp(label);
      await page.locator('h1, h2').filter({ hasText: heading }).first().waitFor({ timeout: 10000 });
      await page.locator('main .loading:visible').waitFor({ state: 'hidden', timeout: 5000 }).catch(() => {});
      await page.screenshot({ path: path.join(__dirname, id + '.png'), animations: 'disabled' });
      const data = await page.evaluate(() => {
        const selectors = ['.page-header', '.page-header h1', '.page-header h2', '.page-header p', '.current-task-strip', '.quick-access-all-button', '.projection-sync-panel', '.workspace-status-panel', '.overview-facts', '.local-usage-panel', '.mission-current-action', '.mission-detail', '.task-center-summary', '.workspace-registry-page', '.workflow-page', '.market-page', '.installed-page'];
        const metrics = selectors.flatMap(selector => [...document.querySelectorAll(selector)].map(element => {
          const rect = element.getBoundingClientRect();
          const style = getComputedStyle(element);
          return { selector, text: element.innerText?.slice(0, 700), x: rect.x, y: rect.y, width: rect.width, height: rect.height, fontSize: style.fontSize, lineHeight: style.lineHeight, padding: style.padding, gap: style.gap, radius: style.borderRadius, shadow: style.boxShadow, overflowX: style.overflowX };
        }));
        const main = document.querySelector('main')?.cloneNode(true);
        main?.querySelectorAll('select, option').forEach(element => element.remove());
        return { viewport: { width: innerWidth, height: innerHeight, dpr: devicePixelRatio, visualScale: visualViewport?.scale }, documentWidth: document.documentElement.scrollWidth, text: main?.textContent?.replace(/\s+/g, ' ').trim() || '', metrics };
      });
      results.push({ id, label, ...data });
      await fs.writeFile(path.join(__dirname, 'desktop-observations.json'), JSON.stringify(results, null, 2));
      console.log(JSON.stringify({ id, viewport: data.viewport, text: data.text.slice(0, 2400), metrics: data.metrics }));
    }
  } finally {
    await browser.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
