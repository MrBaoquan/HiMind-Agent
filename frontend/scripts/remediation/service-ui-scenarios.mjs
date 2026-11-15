import assert from 'node:assert/strict';
import { writeFileSync } from 'node:fs';
import { resolve } from 'node:path';

export async function checkServiceSummaryLayout({ page, evidence, phase }) {
  const metrics = await page.locator('.ai-providers-view').evaluate(element => {
    const bounds = selector => element.querySelector(selector).getBoundingClientRect().toJSON();
    const copy = element.querySelector('.ai-overview-copy');
    return {
      containerWidth: element.clientWidth,
      overview: bounds('.ai-overview'), main: bounds('.ai-overview-main'),
      copy: bounds('.ai-overview-copy'), actions: bounds('.ai-overview-actions'),
      textClientWidth: copy.clientWidth, textScrollWidth: copy.scrollWidth,
    };
  });
  writeFileSync(resolve(evidence, `summary-layout-${phase}.json`), JSON.stringify(metrics, null, 2));
  assert.ok(metrics.copy.width >= Math.min(200, metrics.overview.width - 76), JSON.stringify(metrics));
  assert.ok(metrics.textScrollWidth <= metrics.textClientWidth + 1, JSON.stringify(metrics));
  if (metrics.containerWidth < 480) {
    assert.ok(metrics.actions.top >= metrics.main.bottom - 1, JSON.stringify(metrics));
  }
  return metrics;
}

export async function checkServiceUiExtended({ page, url, evidence }) {
  const checks = [];
  const record = (name, details) => {
    checks.push({ name, details });
    writeFileSync(resolve(evidence, 'extended-assertions.json'), JSON.stringify({ checks }, null, 2));
  };
  await page.goto(url + '?tab=acp');
  await page.locator('#ai-tab-acp[aria-selected="true"]').waitFor();
  assert.equal(await page.locator('#ai-panel-acp').count(), 1);
  await page.getByRole('tab', { name: '运行环境', exact: true }).focus();
  const order = ['mcp', 'services', 'gateway', 'routes', 'acp'];
  await page.keyboard.press('Home');
  for (const tab of order) {
    await page.waitForFunction(tab => document.activeElement.id === `ai-tab-${tab}` && document.activeElement.getAttribute('aria-selected') === 'true', tab);
    await page.keyboard.press('ArrowRight');
  }
  await page.waitForFunction(() => document.activeElement.id === 'ai-tab-mcp' && document.activeElement.getAttribute('aria-selected') === 'true');
  await page.keyboard.press('ArrowLeft');
  await page.waitForFunction(() => document.activeElement.id === 'ai-tab-acp');
  for (const [key, tab] of [['ArrowUp','routes'],['ArrowDown','acp'],['Home','mcp'],['End','acp']]) {
    await page.keyboard.press(key);
    await page.waitForFunction(tab => document.activeElement.id === `ai-tab-${tab}` && document.activeElement.getAttribute('aria-selected') === 'true', tab);
  }
  const ax = await page.context().newCDPSession(page);
  const tree = await ax.send('Accessibility.getFullAXTree');
  assert.equal(tree.nodes.filter(node => !node.ignored && node.role?.value === 'tabpanel').length, 1);
  assert.match(await page.locator('#ai-panel-acp').innerText(), /不改变任务显式选择/);
  await page.getByRole('tab', { name: '能力路由', exact: true }).click();
  assert.match(await page.getByRole('tabpanel').innerText(), /不覆盖已有任务/);
  assert.match(await page.getByRole('tabpanel').innerText(), /跟随服务默认/);
  record('five-tab-all-arrows-home-end-unique-acp-and-default-scope', { tabs: order });

  await page.getByRole('tab', { name: 'AI 服务', exact: true }).click();
  await page.getByText('健康目录已更新；', { exact: false }).waitFor();
  await page.evaluate(() => window.setExtra(true));
  await page.waitForFunction(() => document.querySelectorAll('.ai-service-row').length === 2);
  assert.match(await page.locator('.ai-service-row').last().innerText(), /仅语音 AI 服务/);
  for (const mode of ['error', 'denied']) {
    await page.evaluate(mode => { window.setCatalogMode(mode); window.refresh(); }, mode);
    await page.waitForFunction(mode => document.querySelector('.ai-catalog-state[role="alert"]')?.textContent.includes(mode === 'denied' ? '无权限' : '读取失败'), mode);
    assert.equal(await page.locator('.ai-service-row').count(), 2);
    for (const row of await page.locator('.ai-service-row').all()) assert.match(await row.innerText(), /健康未知/);
    await page.screenshot({ path: resolve(evidence, `full-service-list-${mode}.png`), fullPage: true });
  }
  await page.evaluate(() => window.setCatalogMode('ready'));
  await page.getByRole('button', { name: '重试健康目录' }).click();
  await page.getByText('健康目录已更新；', { exact: false }).waitFor();
  assert.equal(await page.locator('.ai-service-row').count(), 2);
  await page.screenshot({ path: resolve(evidence, 'full-service-list-recovered.png'), fullPage: true });
  record('full-list-chat-and-media-only-survives-catalog-failure-and-local-retry', { count: 2, modes: ['error', 'denied', 'ready'] });
  await page.evaluate(() => window.setExtra(false));
  const trigger = page.getByRole('button', { name: '新增 AI 服务', exact: true });
  await page.locator('.ai-providers-view').evaluate(element => { element.style.width = '296px'; });
  await checkServiceSummaryLayout({ page, evidence, phase: 'nested-296-at-880' });
  await page.screenshot({ path: resolve(evidence, 'summary-nested-296-at-880.png'), fullPage: true });
  await page.locator('.ai-providers-view').evaluate(element => { element.style.removeProperty('width'); });
  record('narrow-summary-readable-width-and-stacked-action-independent-of-shell-container', { width: 296, viewport: 880 });
  await trigger.click();
  const dialog = page.getByRole('dialog');
  await page.getByLabel('服务名称', { exact: true }).fill('保留的非密钥名称');
  await page.getByLabel('API Key', { exact: true }).fill('fake-test-only');
  await page.getByLabel('Base URL', { exact: true }).fill('https://example.test/v1?invalid=1');
  await page.getByLabel('第二 Base URL', { exact: true }).fill('https://secondary.example.test');
  await dialog.getByRole('button', { name: '下一步' }).click();
  assert.equal(await page.locator('#ai-service-modal-title').evaluate(element => element === document.activeElement), true);
  await page.getByLabel('对话默认模型', { exact: true }).fill('retained-model');
  await dialog.getByRole('button', { name: '下一步' }).click();
  await page.evaluate(() => { window.saveError = 'base_url 不应包含 query 或 fragment；请填写 API 根地址'; });
  await dialog.getByRole('button', { name: '保存服务', exact: true }).click();
  await page.locator('#ai-service-error-link').waitFor();
  assert.equal(await page.locator('#ai-service-error-link').evaluate(element => element === document.activeElement), true);
  await page.locator('#ai-service-error-link').click();
  assert.equal(await page.locator('#ai-service-base_url').evaluate(element => element === document.activeElement && element.getAttribute('aria-invalid') === 'true'), true);
  assert.equal(await page.getByLabel('服务名称', { exact: true }).inputValue(), '保留的非密钥名称');
  assert.equal(await page.getByLabel('第二 Base URL', { exact: true }).inputValue(), 'https://secondary.example.test');
  await page.getByLabel('Base URL', { exact: true }).fill('https://example.test/v1');
  await dialog.getByRole('button', { name: '下一步' }).click();
  assert.equal(await page.getByLabel('对话默认模型', { exact: true }).inputValue(), 'retained-model');
  await dialog.getByRole('button', { name: '下一步' }).click();
  await page.evaluate(() => { window.saveError = ''; });
  await dialog.getByRole('button', { name: '保存服务', exact: true }).dblclick();
  await dialog.waitFor({ state: 'detached' });
  assert.equal(await page.evaluate(() => window.saves), 2);
  record('late-server-error-summary-to-early-field-retains-nonsecret-data-and-retry', { field: 'base_url', saves: 2 });

  const layout = [];
  for (const width of [320, 390, 480, 620, 880]) {
    await page.setViewportSize({ width, height: 900 });
    await trigger.click();
    await page.evaluate(() => {
      for (const element of document.querySelectorAll('.ai-service-modal :is(span,small,strong,h3,button)')) {
        for (const text of element.childNodes) {
          if (text.nodeType === Node.TEXT_NODE && text.textContent.trim()) {
            const original = text.textContent;
            text.textContent = original + original.slice(0, Math.ceil(original.length / 2));
          }
        }
      }
    });
    const metrics = await page.locator('dialog').evaluate(element => {
      const bounds = element.getBoundingClientRect();
      return { viewport: innerWidth, width: bounds.width, left: bounds.left, right: bounds.right, client: element.clientWidth, scroll: element.scrollWidth, fields: [...element.querySelectorAll('input,select')].map(field => ({ width: field.getBoundingClientRect().width, client: field.clientWidth, scroll: field.scrollWidth })) };
    });
    layout.push({ width, metrics });
    writeFileSync(resolve(evidence, 'expanded-form-metrics.json'), JSON.stringify(layout, null, 2));
    await page.screenshot({ path: resolve(evidence, `expanded-form-${width}.png`), fullPage: true });
    assert.ok(metrics.left >= 0 && metrics.right <= width + 1 && metrics.scroll <= metrics.client + 1, JSON.stringify(metrics));
    await page.keyboard.press('Escape');
  }
  record('form-copy-50-percent-at-five-widths', layout);
  await page.setViewportSize({ width: 880, height: 900 });
  await trigger.click();
  const tokens = await page.evaluate(() => {
    const style = getComputedStyle(document.documentElement);
    return { meta: style.getPropertyValue('--fs-meta').trim(), body: style.getPropertyValue('--fs-body').trim(), title: style.getPropertyValue('--fs-title').trim(), name: getComputedStyle(document.querySelector('.ai-client-copy strong')).fontSize, heading: getComputedStyle(document.querySelector('.ai-service-modal h3')).fontSize, facts: getComputedStyle(document.querySelector('.ai-service-fact-line')).fontSize };
  });
  assert.deepEqual([tokens.facts, tokens.name, tokens.heading], ['12px', '13px', '14px']);
  record('computed-font-tokens', tokens);
  const contrasts = await page.evaluate(() => {
    const parse = value => {
      const parts = value.match(/[\d.]+/g).map(Number);
      return [parts[0], parts[1], parts[2], parts[3] ?? 1];
    };
    const blend = (front, back) => [...front.slice(0, 3).map((value, index) => value * front[3] + back[index] * (1 - front[3])), 1];
    const background = element => {
      const chain = [];
      for (let current = element; current; current = current.parentElement) chain.push(current);
      return chain.reverse().reduce((color, current) => blend(parse(getComputedStyle(current).backgroundColor), color), [255, 255, 255, 1]);
    };
    const luminance = color => color.slice(0, 3).map(value => {
      const normalized = value / 255;
      return normalized <= .04045 ? normalized / 12.92 : ((normalized + .055) / 1.055) ** 2.4;
    }).reduce((sum, value, index) => sum + value * [.2126, .7152, .0722][index], 0);
    const ratio = (front, back) => {
      const values = [luminance(front), luminance(back)].sort((left, right) => right - left);
      return (values[0] + .05) / (values[1] + .05);
    };
    return [...document.querySelectorAll('.ai-service-modal :is(input,select,button,small,h3,.ai-service-field > span)')].filter(element => element.getClientRects().length && !element.disabled).map(element => {
      const style = getComputedStyle(element), back = background(element), front = blend(parse(style.color), back);
      const border = blend(parse(style.borderTopColor), back);
      return { label: element.getAttribute('id') || element.textContent.trim().slice(0, 60), tag: element.tagName, foreground: front, background: back, textRatio: ratio(front, back), boundaryRatio: ratio(border, background(element.parentElement)), boundaryRequired: element.matches('input,select'), fontSize: style.fontSize, width: element.getBoundingClientRect().width, height: element.getBoundingClientRect().height };
    });
  });
  writeFileSync(resolve(evidence, 'form-contrast-and-boundaries.json'), JSON.stringify(contrasts, null, 2));
  for (const sample of contrasts) {
    assert.ok(sample.textRatio >= 4.5, JSON.stringify(sample));
    if (sample.boundaryRequired) assert.ok(sample.boundaryRatio >= 3, JSON.stringify(sample));
    if (['BUTTON', 'INPUT', 'SELECT'].includes(sample.tag)) assert.ok(sample.width >= 24 && sample.height >= 24, JSON.stringify(sample));
  }
  record('form-text-control-boundaries-and-hit-targets', { samples: contrasts.length, textThreshold: 4.5, boundaryThreshold: 3, minimumTarget: 24 });
  await page.keyboard.press('Escape');
  await page.evaluate(() => { document.body.style.zoom = '2'; });
  writeFileSync(resolve(evidence, 'zoom-before-pointer.json'), JSON.stringify(await trigger.evaluate(element => {
    const bounds = element.getBoundingClientRect();
    return { bounds: bounds.toJSON(), viewport: innerWidth, document: document.documentElement.scrollWidth, hit: document.elementFromPoint(bounds.x + bounds.width / 2, bounds.y + bounds.height / 2)?.outerHTML };
  }), null, 2));
  await page.screenshot({ path: resolve(evidence, 'zoom-before-pointer.png'), fullPage: true });
  await trigger.click({ timeout: 5000 });
  await dialog.waitFor();
  await page.getByLabel('服务名称', { exact: true }).fill('200% 指针操作');
  assert.equal(await page.getByLabel('服务名称', { exact: true }).inputValue(), '200% 指针操作');
  writeFileSync(resolve(evidence, 'zoom-dialog-before-next.json'), JSON.stringify(await dialog.evaluate(element => ({
    zoom: getComputedStyle(document.body).zoom, viewport: [innerWidth, innerHeight],
    elements: [element, ...element.querySelectorAll('.modal-header,.modal-body,.modal-actions,button,input')].map(item => ({tag:item.tagName,label:item.getAttribute('id') || item.textContent.trim().slice(0,40),bounds:item.getBoundingClientRect().toJSON(),client:[item.clientWidth,item.clientHeight],scroll:[item.scrollWidth,item.scrollHeight],overflow:getComputedStyle(item).overflow}))
  })), null, 2));
  await dialog.getByRole('button', { name: '下一步' }).click({ timeout: 5000 });
  await page.locator('#ai-service-error-link').click({ timeout: 5000 });
  assert.equal(await page.locator('#ai-service-api_key').evaluate(element => element === document.activeElement), true);
  const zoomMetrics = await dialog.evaluate(element => ({ viewport: innerWidth, document: document.documentElement.scrollWidth, zoom: getComputedStyle(document.body).zoom, left: element.getBoundingClientRect().left, right: element.getBoundingClientRect().right, client: element.clientWidth, scroll: element.scrollWidth }));
  await page.screenshot({ path: resolve(evidence, 'zoom-200-pointer.png'), fullPage: true });
  assert.ok(zoomMetrics.document <= zoomMetrics.viewport && zoomMetrics.left >= 0 && zoomMetrics.right <= zoomMetrics.viewport + 1 && zoomMetrics.scroll <= zoomMetrics.client + 1, JSON.stringify(zoomMetrics));
  record('200-percent-css-zoom-pointer-and-keyboard', zoomMetrics);
  await page.keyboard.press('Escape');
  await page.evaluate(() => { document.body.style.zoom = '1'; });
  for (const scheme of ['light','dark']) {
    await page.emulateMedia({colorScheme:scheme,reducedMotion:'reduce'});
    await trigger.click();
    const rendered = await dialog.evaluate(element => {
      const style = getComputedStyle(element);
      return {colorScheme:getComputedStyle(document.documentElement).colorScheme,background:style.backgroundColor,motion:[element,...element.querySelectorAll('*')].filter(item=>item.getClientRects().length).map(item=>({transform:getComputedStyle(item).transform,animation:getComputedStyle(item).animationName,transition:getComputedStyle(item).transitionProperty}))};
    });
    for (const sample of rendered.motion) {
      assert.equal(sample.animation,'none');
      assert.equal(sample.transform,'none');
      assert.ok(['opacity','none','all'].includes(sample.transition));
    }
    await page.screenshot({path:resolve(evidence,`${scheme}-reduced-motion.png`),fullPage:true});
    record(`${scheme}-reduced-motion-local-light-theme`,rendered);
    await page.keyboard.press('Escape');
  }
  await page.emulateMedia({colorScheme:'light',reducedMotion:'no-preference'});
}
