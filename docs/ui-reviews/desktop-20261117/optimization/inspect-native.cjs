// Read-only inspection of the signed development Agent's real WebView2 windows.
const {chromium}=require('F:/WebProjects/项目看板/himind-agent/target/session-history-playwright/node_modules/playwright');
const fs=require('node:fs/promises');
const path=require('node:path');
const assert=require('node:assert/strict');
async function snapshot(page,id){
  const result=await page.evaluate(()=>{
    const selectors=['.page-header h1','.workspace-registry-head h2','.overview-facts','.inbox-summary','.inbox-grid','.inbox-section','.mission-task-entry','.workflow-list-issue','.quick-access-all-button','.quick-access-all-button .quick-access-button-label','.workspace-status-panel','.ai-overview'];
    const metrics=selectors.flatMap(selector=>[...document.querySelectorAll(selector)].map(e=>{
      const r=e.getBoundingClientRect(),s=getComputedStyle(e);
      return {selector,text:e.innerText?.slice(0,240),x:r.x,y:r.y,width:r.width,height:r.height,clientWidth:e.clientWidth,scrollWidth:e.scrollWidth,fontSize:s.fontSize,fontWeight:s.fontWeight,lineHeight:s.lineHeight,animation:s.animation,shadow:s.boxShadow};
    }));
    const ready=[...document.querySelectorAll('.workspace-status-panel.ready,.workspace-status-icon.ready,.workspace-status-pill.ready i')].flatMap(e=>[null,'::before','::after'].map(p=>({element:e.className,pseudo:p,animation:getComputedStyle(e,p).animationName,shadow:getComputedStyle(e,p).boxShadow})));
    return {viewport:{width:innerWidth,height:innerHeight,dpr:devicePixelRatio,scale:visualViewport.scale},documentWidth:document.documentElement.scrollWidth,scrollTop:document.querySelector('.main-content')?.scrollTop,metrics,ready,dialogs:[...document.querySelectorAll('[role=dialog]')].filter(e=>e.getBoundingClientRect().height).map(e=>e.innerText.slice(0,240))};
  });
  await page.screenshot({path:path.join(__dirname,id+'.png')});
  return {id,...result};
}
(async()=>{
 const browser=await chromium.connectOverCDP('http://127.0.0.1:9312');
 const results=[],checks=[],errors=[];
 try{
  const pages=browser.contexts().flatMap(c=>c.pages());
  let main;
  for(const p of pages)if(await p.getByRole('navigation',{name:'主导航'}).count())main=p;
  assert.ok(main,'real Agent main window');
  await main.reload();
  await main.getByRole('navigation',{name:'主导航'}).waitFor();
  const conversationHost=await main.locator('.builtin-ai-page-host').elementHandle();
  main.on('pageerror',e=>errors.push(e.message));
  const nav=main.getByRole('navigation',{name:'主导航'});
  const targets=[['01-home','首页',/首页|Agent 状态/],['02-tasks','我的任务',/我的任务/],['03-inbox','待处理',/需要我处理/],['04-workspaces','工作区',/工作区/],['05-workflows','工作流',/工作流/],['06-schedules','定时计划',/定时计划/],['07-market','市场',/市场/],['08-installed','我的能力',/我的能力/],['09-development','开发工作区',/扩展开发|开发工作区/]];
  for(const [id,label,heading]of targets){
   await nav.getByRole('button',{name:label,exact:true}).click();
   await main.locator('h1,h2').filter({hasText:heading}).first().waitFor({timeout:15000});
   if(id==='02-tasks'){
    await main.locator('.task-center-history').waitFor({timeout:15000});
    await main.locator('.task-center-loading').waitFor({state:'hidden',timeout:15000});
    await main.waitForFunction(()=>document.querySelector('.main-content')?.scrollTop===0,null,{timeout:10000});
   }
   const data=await snapshot(main,id);results.push(data);
   assert.equal(data.viewport.scale,1);assert.equal(data.documentWidth,data.viewport.width);
   assert.equal(data.scrollTop,0,`${id}: primary navigation scroll`);
   const title=data.metrics.find(m=>m.selector==='.page-header h1'||m.selector==='.workspace-registry-head h2');
   assert.ok(title,`${id}: page title`);assert.equal(title.fontSize,'20px');assert.equal(title.fontWeight,'600');assert.equal(title.lineHeight,'26px');
   const tool=data.metrics.find(m=>m.selector==='.quick-access-all-button .quick-access-button-label');
   if(tool)assert.ok(tool.scrollWidth<=tool.clientWidth,'full tool label');
   if(id==='01-home'){
    const facts=data.metrics.find(m=>m.selector==='.overview-facts');assert.match(facts.text,/工作台账号/);assert.match(facts.text,/工作台任务/);
    if(data.ready.length){
     assert.ok(data.ready.every(r=>r.animation==='none'),'ready is static');
     await main.waitForTimeout(10000);
     const after=await snapshot(main,'01-home-after-10s');results.push(after);
     assert.deepEqual(after.ready,data.ready);checks.push('native-ready-static-for-10s');
    }
    checks.push('scoped-workbench-status');
   }
   if(id==='03-inbox'){
    const summary=data.metrics.find(m=>m.selector==='.inbox-summary');
    if(summary){assert.ok(summary.height>=76);checks.push('native-inbox-summary-height');}
    else {await main.getByText('当前没有待处理事项',{exact:true}).waitFor();checks.push('native-empty-inbox');}
   }
   await fs.writeFile(path.join(__dirname,'native-result.json'),JSON.stringify({passed:false,checks,results,errors},null,2));
   if(data.dialogs.length)throw new Error(`${id}: unexpected dialog, stopped without changing it`);
   // Prove a long page's scroll cannot leak into the next navigation target.
   if(id==='02-tasks'||id==='03-inbox'||id==='04-workspaces'){
    data.departureScrollTop=await main.locator('.main-content').evaluate(e=>{e.scrollTop=400;return e.scrollTop});
    if(id==='02-tasks')assert.ok(data.departureScrollTop>0,'native task list was scrolled before Inbox navigation');
   }
  }
  assert.ok(await conversationHost.evaluate(e=>e.isConnected),'persistent conversation host stays mounted');
  checks.push('native-conversation-host-preserved');
  checks.push('nine-native-pages-titles-and-navigation','native-tool-label-complete');
  await main.getByRole('button',{name:'打开账号菜单',exact:true}).click();
  await main.getByRole('menuitem',{name:/设置.*连接、权限/}).click();
  let settings;
  for(let attempt=0;attempt<30&&!settings;attempt++){
   for(const p of browser.contexts().flatMap(c=>c.pages()))if(await p.locator('.settings-window-root').count())settings=p;
   if(!settings)await main.waitForTimeout(200);
  }
  assert.ok(settings,'real native settings window');
  settings.on('pageerror',e=>errors.push(e.message));
  await settings.getByRole('navigation',{name:'设置分类'}).getByRole('button',{name:'AI 连接',exact:true}).click();
  await settings.getByRole('tab',{name:'AI 服务',exact:true}).click();
  await settings.locator('.ai-overview[aria-busy=false]').waitFor({timeout:20000});
  const ai=await snapshot(settings,'10-ai-services');results.push(ai);
  assert.equal(ai.viewport.scale,1);assert.equal(ai.documentWidth,ai.viewport.width);
  const aiTitle=ai.metrics.find(m=>m.selector==='.page-header h1');assert.equal(aiTitle.fontSize,'20px');
  assert.ok(ai.metrics.find(m=>m.selector==='.ai-overview').height>=86);
  checks.push('native-settings-ai-services-loaded');
  assert.deepEqual(errors,[]);
  await fs.writeFile(path.join(__dirname,'native-result.json'),JSON.stringify({passed:true,checks,results,errors},null,2));
  console.log(JSON.stringify({passed:true,checks,pages:results.map(r=>({id:r.id,viewport:r.viewport,scrollTop:r.scrollTop,rows:r.metrics.filter(m=>m.selector==='.mission-task-entry').length,summary:r.metrics.find(m=>m.selector==='.inbox-summary')?.height})),errors}));
 }catch(error){await fs.writeFile(path.join(__dirname,'native-failure.json'),JSON.stringify({error:String(error),checks,results,errors},null,2));throw error;}
 finally{await browser.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
