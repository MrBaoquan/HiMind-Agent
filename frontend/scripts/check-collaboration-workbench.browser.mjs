import assert from 'node:assert/strict';
import {mkdirSync,writeFileSync} from 'node:fs';
import {dirname,resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {createServer} from 'vite';
import react from '@vitejs/plugin-react';
import {chromium} from 'playwright';
import {createRuntime} from './remediation/service-ui-runtime.mjs';
const root=resolve(dirname(fileURLToPath(import.meta.url)),'..');
const runtime=createRuntime('browser');
const evidence=resolve(root,'../.himind/long-tasks/20261117-sustainable-collaboration-workbench/evidence/desktop-workbench');
mkdirSync(evidence,{recursive:true});
const server=await createServer({root,configFile:false,cacheDir:runtime.cache,plugins:[react()],server:{host:'127.0.0.1',port:0}});
await server.listen();
let browser;
try{browser=await chromium.launch({headless:true});}catch{browser=await chromium.launch({headless:true,channel:'msedge'});}
const errors=[],checks=[];
try{
 const page=await browser.newPage({viewport:{width:1440,height:900}});
 page.on('pageerror',e=>errors.push(e.message));
 const url='http://127.0.0.1:'+server.httpServer.address().port+'/scripts/fixtures/collaboration-workbench.html';
 await page.goto(url); await page.locator('.mission-work-card').first().waitFor();
 await page.getByRole('button',{name:'与协调助手沟通',exact:true}).click();
 const input=page.getByRole('textbox',{name:'发给协调助手的消息'});
 const main=page.locator('.mission-workspace-main'),aside=page.locator('.mission-workspace-aside');
 async function inWindow(locator){const box=await locator.boundingBox();assert.ok(box&&box.y>=0&&box.y+box.height<=page.viewportSize().height+1,JSON.stringify(box));}
 async function refreshUI(){const menu=page.locator('.mission-more-actions');if(await menu.getAttribute('open')===null)await menu.locator('summary').click();await menu.getByRole('button',{name:'刷新任务',exact:true}).click();if(await menu.getAttribute('open')!==null)await menu.locator('summary').click();}
 async function noPageOverflow(){assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),true);assert.equal(await page.locator('.main-content').evaluate(el=>el.scrollHeight<=el.clientHeight+1),true);}
 const pane=await aside.boundingBox(),board=await main.boundingBox();
 assert.ok(pane.x>=board.x+board.width,'desktop context pane must dock to the right');
 await inWindow(input); await noPageOverflow();
 assert.match(await page.locator('.mission-detail-progress').innerText(),/2 项实际执行/);
 checks.push('desktop-dock-fills-window-with-visible-composer');
 await input.fill('待发送的离线需求草稿');
 await page.locator('[data-task-id="ui"]').click();
 assert.equal(await page.locator('.mission-workspace-aside').count(),1);
 assert.equal(await page.locator('.mission-desktop-workspace').getAttribute('data-panel'),'detail');
 assert.equal(await page.locator('[data-mission-task-id]').count(),1,'task card opens its own details');
 await page.getByRole('button',{name:'与协调助手沟通',exact:true}).click();
 assert.equal(await input.inputValue(),'待发送的离线需求草稿');
 checks.push('single-context-dock-and-draft-retained');
 await input.fill('为什么测试还没开始');
 const before=await page.evaluate(()=>window.workbenchFixture.revision);
 await page.getByRole('button',{name:'发送',exact:true}).click();
 await page.waitForFunction(()=>window.workbenchFixture.submissions.length===1);
 await page.waitForFunction(()=>!document.querySelector('.mission-coordination textarea').value);
 assert.equal(await page.evaluate(()=>window.workbenchFixture.revision),before);
 assert.match(await page.locator('.mission-conversation').innerText(),/询问未修改计划/);
 checks.push('question-routes-without-plan-change');
 await page.evaluate(()=>{window.workbenchFixture.addMessages(45);window.workbenchFixture.setScene('many')});
 await page.locator('.mission-upcoming-work summary').waitFor();
 assert.equal(await page.locator('.mission-upcoming-work').getAttribute('open'),null);
 await page.locator('.mission-upcoming-work summary').click();
 await page.waitForFunction(()=>document.querySelectorAll('.mission-work-card').length>60);
 await page.getByRole('button',{name:'关闭面板',exact:true}).click();
 await page.getByRole('button',{name:'与协调助手沟通',exact:true}).click();
 await main.evaluate(el=>{el.scrollTop=600});
 assert.ok(await main.evaluate(el=>el.scrollTop)>500);
 await inWindow(input); await noPageOverflow();
 const position=await page.locator('.mission-conversation').evaluate(el=>{el.scrollTop=320;return el.scrollTop});
 await page.getByRole('button',{name:'查看计划、团队与活动记录',exact:true}).click();
 await page.getByRole('button',{name:'与协调助手沟通',exact:true}).click();
 assert.ok(Math.abs(await page.locator('.mission-conversation').evaluate(el=>el.scrollTop)-position)<2);
 await page.screenshot({path:resolve(evidence,'desktop-many-tasks.png')});
 checks.push('independent-scroll-and-transcript-position');
 for(const [width,height] of [[1280,800],[1024,720],[800,640],[650,550]]){
  await page.setViewportSize({width,height}); await inWindow(input); await noPageOverflow();
  await page.screenshot({path:resolve(evidence,`window-${width}x${height}.png`)});
 }
 checks.push('compact-window-overlay-and-composer-visible');
 await page.setViewportSize({width:1440,height:900});
 for(const scene of ['running','waiting','confirmation','verification','failed','completed']){
  await page.evaluate(scene=>window.workbenchFixture.setScene(scene),scene);
  await refreshUI();
  await page.waitForTimeout(150);
  if(scene==='confirmation'){
   await page.getByRole('button',{name:'查看并确认变更',exact:true}).waitFor();
   await inWindow(page.getByRole('button',{name:'查看并确认变更',exact:true}));
   await page.getByRole('button',{name:'查看并确认变更',exact:true}).click();
   assert.match(await page.locator('.mission-change-updates').innerText(),/整合与独立验收/);
   assert.match(await page.locator('.mission-change-updates').innerText(),/实现离线存储和恢复/);
   assert.match(await page.locator('.mission-change-updates').innerText(),/node --test/);
   assert.match(await page.locator('.mission-change-review').innerText(),/断网后仍能读取已保存内容/);
   await inWindow(page.getByRole('button',{name:'确认并应用这项变更',exact:true}));
   await page.getByRole('button',{name:'确认并应用这项变更',exact:true}).click();
   await page.waitForFunction(()=>window.workbenchFixture.revision===4);
  }
  if(scene==='completed'){
   assert.equal(await page.getByRole('tab',{name:'交付',exact:true}).getAttribute('aria-selected'),'true');
   await page.locator('.mission-workflow-save summary').click();
   await page.getByRole('button',{name:'保存到我的能力',exact:true}).click();
   await page.waitForFunction(()=>window.workbenchFixture.savedWorkflow?.missionId==='fixture-workbench');
   assert.match(await page.locator('.collaboration-export-notice').innerText(),/已保存到“我的能力”/);
  }
  await noPageOverflow();
  await page.screenshot({path:resolve(evidence,`state-${scene}.png`)});
 }
 checks.push('real-component-execution-wait-confirm-verification-failure-completion');
 await page.evaluate(()=>window.workbenchFixture.setScene('disconnected'));
 await refreshUI();
 await page.locator('.mission-connection-status').waitFor();
 assert.match(await page.locator('.mission-detail-head .collaboration-task-status').innerText(),/待核实/);
 assert.match(await page.locator('.mission-detail-progress').innerText(),/待核实/);
 await page.screenshot({path:resolve(evidence,'state-disconnected.png')});
 checks.push('disconnected-does-not-claim-live-concurrency');
 assert.deepEqual(errors,[]);
 writeFileSync(resolve(evidence,'result.json'),JSON.stringify({kind:'production_components_in_memory_host_fixture',passed:true,checks,pageErrors:errors,runtime:runtime.directory},null,2));
 console.log('Desktop collaboration workbench: '+checks.length+' behavior/layout groups passed (fixture, not installed runtime).');
}finally{await browser.close();await server.close();}
