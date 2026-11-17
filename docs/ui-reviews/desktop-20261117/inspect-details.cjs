const { chromium } = require('../../../target/session-history-playwright/node_modules/playwright');
const fs = require('node:fs/promises');
const path = require('node:path');

(async () => {
  const browser = await chromium.connectOverCDP('http://127.0.0.1:9311');
  const records = [];
  try {
    const pages = browser.contexts().flatMap(context => context.pages());
    let main, settings;
    for (const page of pages) {
      if (await page.locator('nav[aria-label="主导航"]').count()) main = page;
      if (await page.locator('.settings-window-root').count()) settings = page;
    }
    if (!main) throw new Error('Main window not found');
    async function capture(page, id, selectors = []) {
      await page.screenshot({ path: path.join(__dirname, id + '.png'), animations: 'disabled' });
      const record = await page.evaluate(selectors => {
        const rect = element => { const r = element.getBoundingClientRect(); const s = getComputedStyle(element); return { class: element.className, text: element.innerText?.slice(0, 500), x:r.x,y:r.y,width:r.width,height:r.height,font:s.fontSize,lineHeight:s.lineHeight,gap:s.gap,padding:s.padding,shrink:s.flexShrink,columns:s.gridTemplateColumns,overflow:s.overflow,animation:s.animation,shadow:s.boxShadow }; };
        return { viewport:{width:innerWidth,height:innerHeight,dpr:devicePixelRatio,visualScale:visualViewport?.scale}, text:document.querySelector('main')?.innerText, scrolls:[...document.querySelectorAll('*')].filter(e=>e.scrollTop>0).map(e=>({class:e.className,top:e.scrollTop,height:e.clientHeight,scroll:e.scrollHeight})), metrics:selectors.flatMap(selector=>[...document.querySelectorAll(selector)].map(element=>({selector,...rect(element)}))), images:[...document.querySelectorAll('img')].map(e=>({alt:e.alt,complete:e.complete,naturalWidth:e.naturalWidth,scheme:e.src.split(':')[0],width:e.width,height:e.height})), stylesheets:[...document.styleSheets].map(sheet=>sheet.href) };
      }, selectors);
      records.push({ id, ...record });
      await fs.writeFile(path.join(__dirname, 'desktop-details.json'), JSON.stringify(records,null,2));
      console.log(JSON.stringify({id,viewport:record.viewport,metrics:record.metrics,text:record.text?.slice(0,1400),images:record.images}));
    }
    if (settings) {
      await capture(settings,'10-settings-general',['.settings-window-pane','.page-header h1','.settings-section h3','.setting-row']);
      await settings.getByRole('navigation',{name:'设置分类'}).getByRole('button',{name:'AI 连接',exact:true}).click();
      await settings.getByRole('tab',{name:'AI 服务',exact:true}).click();
      await settings.locator('.ai-providers-view').waitFor();
      await capture(settings,'11-ai-services',['.settings-window-pane','.ai-overview','.ai-service-card','.ai-service-row','.page-header','.ai-tabs']);
    }
    const nav=main.getByRole('navigation',{name:'主导航'});
    await nav.getByRole('button',{name:'待处理',exact:true}).click();
    await main.locator('.inbox-grid').waitFor();
    await main.locator('.main-content').evaluate(e=>e.scrollTo(0,0));
    await capture(main,'03-inbox-top',['.main-content','.page-header','.inbox-summary','.inbox-summary > div','.inbox-grid','.inbox-section','.inbox-item']);
    await nav.getByRole('button',{name:'我的任务',exact:true}).click();
    await main.locator('.mission-task-entry').first().waitFor();
    await main.locator('.main-content').evaluate(e=>e.scrollTo(0,0));
    await capture(main,'02-tasks-top',['.task-center-row','.task-center-row-meta','.task-row-next','.task-center-filters']);
    await main.locator('.main-content').evaluate(e=>e.scrollTo(0,500));
    await nav.getByRole('button',{name:'待处理',exact:true}).click();
    await main.locator('.inbox-grid').waitFor();
    await capture(main,'12-navigation-scroll',['.main-content','.page-header','.inbox-summary']);
    await nav.getByRole('button',{name:'我的能力',exact:true}).click();
    await main.waitForFunction(()=>!document.querySelector('main')?.innerText.includes('正在读取技能'),null,{timeout:20000});
    await capture(main,'08-installed',['.installed-page','.skill-toolbar-row','.skill-client-summary-compact']);
    await nav.getByRole('button',{name:'市场',exact:true}).click();
    await main.waitForFunction(()=>!document.querySelector('main')?.innerText.includes('版本读取中'),null,{timeout:20000}).catch(()=>{});
    await capture(main,'07-market',['.market-layout','.market-list','.market-detail','.extension-icon-frame','.extension-icon-art']);
    await nav.getByRole('button',{name:'工作流',exact:true}).click();
    await main.locator('.workflow-page').waitFor();
    await capture(main,'05-workflows',['.workflow-page','.workflow-layout','.workflow-load-error','.workflow-list-error']);
    await nav.getByRole('button',{name:'首页',exact:true}).click();
    await main.locator('.overview-facts').waitFor();
    await main.locator('.main-content').evaluate(e=>e.scrollTo(0,0));
    await capture(main,'01-home-top',['.workspace-status-panel','.workspace-status-body','.ai-usage-readout','.ai-usage-readout > span','.ai-usage-readout small','.quick-access-all-button','.quick-access-all-button .quick-access-button-label']);
  } finally {
    await browser.close();
  }
})().catch(error=>{console.error(error);process.exitCode=1;});
