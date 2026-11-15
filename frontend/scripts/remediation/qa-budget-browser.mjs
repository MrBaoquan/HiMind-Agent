import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {createInterface} from 'node:readline';
import {readFileSync,writeFileSync,appendFileSync,mkdirSync} from 'node:fs';
import {resolve} from 'node:path';
import {createServer} from 'vite';
import react from '@vitejs/plugin-react';
import {chromium} from 'playwright';
import {createRuntime,workspace} from './service-ui-runtime.mjs';

const runtime=createRuntime('browser');
// Use the Windows short alias of the same authorized directory. Chromium profile
// names exceed MAX_PATH under the host's two content-hash directory components.
const shortTemporary=execFileSync('python',['-c','import ctypes,sys; b=ctypes.create_unicode_buffer(32768); assert ctypes.windll.kernel32.GetShortPathNameW(sys.argv[1],b,len(b)); print(b.value)',runtime.temporary],{encoding:'utf8',windowsHide:true}).trim();
Object.assign(process.env,{TEMP:shortTemporary,TMP:shortTemporary,TMPDIR:shortTemporary});
const evidence=runtime.directory;
const state=resolve(evidence,'state');mkdirSync(state);
const executable=resolve(process.env.SERVICE_UI_RUNTIME_ROOT,'probe/t/debug/mission-ui-store-probe.exe');
const processProbe=spawn(executable,[state],{stdio:['pipe','pipe','pipe'],windowsHide:true});
const pending=[];let serial=Promise.resolve();let probeErrors='';
processProbe.stderr.on('data',data=>{probeErrors+=data});
createInterface({input:processProbe.stdout}).on('line',line=>{const entry=pending.shift();if(entry) entry(JSON.parse(line));});
const transcript=resolve(evidence,'interface.jsonl');
function call(request){
 const next=serial.then(()=>new Promise((accept,reject)=>{
  const timer=setTimeout(()=>reject(new Error('Store probe timeout: '+request.command)),15000);
  pending.push(response=>{clearTimeout(timer);appendFileSync(transcript,JSON.stringify({at:new Date().toISOString(),request,response})+'\n');accept(response);});
  processProbe.stdin.write(JSON.stringify(request)+'\n');
 }));serial=next.catch(()=>{});return next;
}
const seed=await call({command:'seed'});assert.equal(seed.ok,true,JSON.stringify(seed));const ids=seed.value;
writeFileSync(resolve(evidence,'scenario-ids.json'),JSON.stringify(ids,null,2));
let dropNextBudget=true;let snapshotMismatch=false;
const snapshots=[];
const server=await createServer({root:resolve(workspace,'frontend'),cacheDir:runtime.cache,configFile:false,plugins:[react(),{
 name:'isolated-real-mission-store',transformIndexHtml(html){return html.replace('<head>','<head>'+'<script type="module">import RefreshRuntime from "/@react-refresh";RefreshRuntime.injectIntoGlobalHook(window);window.$RefreshReg$=()=>{};window.$RefreshSig$=()=>type=>type;window.__vite_plugin_react_preamble_installed__=true;</script>');},configureServer(server){server.middlewares.use('/ui-store-probe',async(req,res)=>{
  try{
   const chunks=[];for await(const chunk of req)chunks.push(chunk);const value=JSON.parse(Buffer.concat(chunks).toString());
   const {mode,...request}=value;
   let response;
   if(mode==='loading')await new Promise(accept=>setTimeout(accept,1800));
   if(mode==='error'||mode==='denied')response={ok:false,error:mode==='denied'?'无权读取任务（隔离传输样本）':'任务读取失败（隔离传输样本）'};
   else if(mode==='empty'&&request.command==='list_missions')response={ok:true,value:[]};
   else response=await call(request);
   if(response.ok&&request.command==='get_mission_snapshot'){
    snapshots.push(response.value);
    if(snapshotMismatch)response={...response,value:{...response.value,mission:{...response.value.mission,effective_wait:{...response.value.mission.effective_wait,cursor:response.value.cursor+1}}}};
   }
   if(response.ok&&request.command==='increase_mission_budget'&&dropNextBudget){
    dropNextBudget=false;
    // Real mutation succeeded, then a separate real pause occurred before retry.
    await call({command:'control_mission',missionId:request.missionId,action:'pause'});
    response={ok:false,error:'隔离网络故障：增额响应丢失，请按原参数重试'};
   }
   res.setHeader('Content-Type','application/json; charset=utf-8');res.end(JSON.stringify(response));
  }catch(error){res.statusCode=500;res.end(JSON.stringify({ok:false,error:String(error)}));}
 });}
}],server:{host:'127.0.0.1',port:0}});
await server.listen();const address=server.httpServer.address();const url=`http://127.0.0.1:${address.port}/scripts/fixtures/mission-budget.html`;
let browser;let page;const assertions=[];const errors=[];
function checked(name){assertions.push(name);console.log('PASS '+name)}
try{
 try{browser=await chromium.launch({headless:true,...(process.env.CHROMIUM_EXECUTABLE?{executablePath:process.env.CHROMIUM_EXECUTABLE}:{})});}
 catch(error){writeFileSync(resolve(evidence,'chromium-default-failure.txt'),String(error));browser=await chromium.launch({headless:true,channel:'msedge'});}
 page=await browser.newPage({viewport:{width:1280,height:900}});page.on('pageerror',error=>errors.push(error.message));
 const capture=async(name)=>{await page.screenshot({path:resolve(evidence,name+'.png'),fullPage:true});};
 const tasks=async()=>{await page.getByRole('button',{name:'任务入口',exact:true}).click();await page.locator('[data-task-id]').first().waitFor();};
 const open=async(name)=>{await tasks();await page.locator(`[data-task-id="mission:${ids[name]}"]`).getByRole('button',{name,exact:true}).click();await page.locator('.mission-detail[data-mission-id]').waitFor();};
 const noOverflow=async()=>assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1),true);
 await page.goto(url);await page.locator('[data-task-id]').first().waitFor();
 const row=page.locator(`[data-task-id="mission:${ids.budget}"]`);
 assert.match(await row.innerText(),/预算已用尽/);const cursor=await row.getAttribute('data-snapshot-cursor');const todo=await row.getAttribute('data-todo-id');
 await capture('budget-list');
 await page.getByRole('button',{name:'待处理入口'}).click();const inbox=page.locator(`[data-todo-id="${todo}"]`);await inbox.waitFor();
 assert.equal(await inbox.getAttribute('data-snapshot-cursor'),cursor);assert.equal(await inbox.getAttribute('data-wait-reason'),'budget_exhausted');assert.equal(await inbox.getAttribute('data-wait-actor'),'user');
 assert.equal(await page.getByText('pause',{exact:true}).count(),0);assert.equal(await page.getByText('verification',{exact:true}).count(),0);
 await capture('budget-inbox');await inbox.click();await page.locator('.mission-detail[data-mission-id]').waitFor();
 assert.equal(await page.locator('.mission-detail').getAttribute('data-snapshot-cursor'),cursor);
 assert.equal(await page.locator('.mission-detail').getAttribute('data-wait-reason'),'budget_exhausted');
 const before=(await call({command:'get_mission_snapshot',missionId:ids.budget})).value;
 assert.equal(before.events.at(-1).event_type,'task_progress');checked('budget_exhausted followed by task_progress: same DTO across list/inbox/detail');
 await page.getByRole('button',{name:'增加预算并继续',exact:true}).click();await page.getByRole('dialog').waitFor();
 await page.getByRole('dialog').getByRole('button',{name:'关闭',exact:true}).focus();
 for(let i=0;i<12;i++){await page.keyboard.press('Tab');assert.equal(await page.evaluate(()=>document.activeElement===document.body || document.querySelector('dialog').contains(document.activeElement)),true);}
 await page.getByLabel('追加 Token',{exact:true}).fill('50');
 assert.equal(await page.getByLabel('追加时长（分钟）').inputValue(),'0');
 await page.getByRole('button',{name:'确认并继续'}).click();await page.getByRole('alert').filter({hasText:'预算更新失败'}).waitFor();await capture('lost-response');
 await page.getByRole('dialog').getByRole('button',{name:'关闭',exact:true}).click();
 await page.locator('.mission-more-actions > summary').click();await page.getByRole('button',{name:'重试原增额申请'}).click();await page.getByRole('dialog').waitFor();
 assert.equal(await page.getByLabel('追加 Token',{exact:true}).inputValue(),'50');
 await page.getByRole('button',{name:'确认并继续'}).click();await page.getByRole('dialog').waitFor({state:'hidden'});
 const after=(await call({command:'get_mission_snapshot',missionId:ids.budget})).value;
 assert.equal(after.mission.budget.max_total_tokens,150);assert.equal(after.mission.status,'waiting');assert.equal(after.mission.effective_wait.reason,'manual_pause');
 const extensions=after.events.filter(event=>event.event_type==='budget_extended');assert.equal(extensions.length,1);
 const requests=readFileSync(transcript,'utf8').trim().split('\n').map(JSON.parse).filter(record=>record.request.command==='increase_mission_budget');
 assert.equal(requests.length,2);assert.deepEqual(requests[0].request,requests[1].request);
 assert.deepEqual(after.mission.confirmation,before.mission.confirmation);assert.equal(after.mission.plan_sha256,before.mission.plan_sha256);
 assert.equal(after.events.filter(e=>e.event_type==='task_dispatched').length,before.events.filter(e=>e.event_type==='task_dispatched').length);
 await capture('retry-current-pause');checked('real SQLite increase, lost response, dialog reopen, identical request key, once-only extension, later pause preserved');
 const conflict=await call({...requests[0].request,input:{...requests[0].request.input,additional_tokens:60}});assert.equal(conflict.ok,false);
 const conflictAfter=(await call({command:'get_mission_snapshot',missionId:ids.budget})).value;assert.deepEqual(conflictAfter,after);checked('same idempotency key with different parameters rejected without mutation');
 // The environment remains blocking after a sufficient real budget increase.
 await open('environment');await page.getByRole('button',{name:'增加预算并继续',exact:true}).count();
 await page.locator('.mission-more-actions > summary').click();await page.getByRole('button',{name:'主动增加预算并续跑'}).click();
 await page.getByLabel('追加 Token',{exact:true}).fill('50');await page.getByRole('button',{name:'确认并继续'}).click();await page.getByRole('dialog').waitFor({state:'hidden'});
 const environment=(await call({command:'get_mission_snapshot',missionId:ids.environment})).value;
 assert.equal(environment.mission.status,'waiting');assert.equal(environment.mission.effective_wait.reason,'environment_blocked');
 assert.equal(environment.mission.effective_wait.can_resume,false);await page.getByRole('button',{name:'处理命令环境'}).waitFor();await capture('environment-blocked');checked('sufficient budget does not bypass environment');
 await open('pause');assert.match(await page.locator('.mission-current-action').innerText(),/已暂停|由你决定/);assert.equal(await page.locator('.mission-stage-rail.is-live').count(),0);await capture('manual-pause');checked('manual pause is optional user choice, excluded from inbox');
 await open('verification');assert.match(await page.locator('.mission-current-action').innerText(),/已派发|待核验/);assert.equal(await page.locator('.mission-stage-rail.is-live').count(),0);assert.equal(await page.locator('.mission-current-action .busy-indicator').count(),0);await capture('verification-dispatched');checked('dispatched verification is not fabricated live work');
 await open('coordination');assert.match(await page.locator('.mission-current-action').innerText(),/等待系统更新/);assert.equal(await page.locator('.mission-detail').getAttribute('data-todo-id'),null);await page.getByRole('tab',{name:'活动记录'}).click();assert.match(await page.locator('[role=tabpanel]').innerText(),/收到审查意见|重试整理/);await capture('coordination-retry');checked('coordination and feedback/retry labels do not invent a worker or require user action');
 await open('partial');await page.getByRole('heading',{name:'交付结果与验收'}).waitFor();assert.match(await page.locator('.mission-detail-progress').innerText(),/1\/2/);
 await page.locator('.mission-delivery-group > summary').first().click();
 const delivery=page.locator('.mission-delivery');assert.match(await delivery.innerText(),/总体尚未完成/);assert.match(await delivery.innerText(),/未提供正文/);assert.match(await delivery.innerText(),/不推测通过数量|未提供正文/);assert.match(await delivery.innerText(),/partial-implementation/);assert.match(await delivery.innerText(),/partial-review-contract-seed/);
 assert.equal(await delivery.getByRole('heading',{name:'实现结果',exact:true}).count(),1);assert.equal(await delivery.getByRole('heading',{name:'必需检查',exact:true}).count(),1);assert.equal(await delivery.getByRole('heading',{name:'独立验收',exact:true}).count(),1);
 await delivery.scrollIntoViewIfNeeded();await capture('partial-delivery');await page.locator('.mission-evidence-columns').first().screenshot({path:resolve(evidence,'partial-evidence-columns.png')});checked('partial delivery exposes implementation/check/review with separate run/id/time and missing bodies');
 await page.setViewportSize({width:390,height:844});await delivery.scrollIntoViewIfNeeded();await noOverflow();await capture('partial-390');
 await page.setViewportSize({width:320,height:800});await delivery.scrollIntoViewIfNeeded();await noOverflow();await capture('partial-320');
 // CSS zoom renders text and controls at an actual 200% scale; 640px also
 // exercises equivalent responsive layout. Native browser chrome zoom is unverified.
 await page.setViewportSize({width:1280,height:900});await page.evaluate(()=>document.documentElement.style.zoom='200%');
 await delivery.scrollIntoViewIfNeeded();await noOverflow();
 const zoomState=await page.evaluate(()=>({innerWidth,devicePixelRatio,cssZoom:getComputedStyle(document.documentElement).zoom}));
 assert.equal(Number(zoomState.cssZoom),2);await capture('partial-200-percent');
 await page.evaluate(()=>document.documentElement.style.zoom='');await page.setViewportSize({width:640,height:900});await noOverflow();await capture('partial-200-percent-layout');
 await page.emulateMedia({colorScheme:'dark',reducedMotion:'reduce'});await noOverflow();await capture('dark-preference-reduced');
 const reduced=await page.locator('.mission-detail').evaluate(el=>({animations:el.getAnimations({subtree:true}).map(animation=>({playState:animation.playState,timing:animation.effect?.getTiming()})),background:getComputedStyle(el).backgroundColor}));
 assert.equal(reduced.animations.filter(a=>a.playState==='running'&&a.timing?.iterations===Infinity).length,0);checked('320/390px, long paths, 200% CSS zoom and equivalent layout, dark preference, reduced motion');
 await open('running');await page.emulateMedia({reducedMotion:'no-preference'});assert.ok(await page.locator('.mission-stage-rail.is-live').count());await capture('running-feedback');
 await page.emulateMedia({reducedMotion:'reduce'});const runningAnimations=await page.locator('.mission-detail').evaluate(el=>el.getAnimations({subtree:true}).filter(a=>a.playState==='running'&&a.effect?.getTiming().iterations===Infinity).length);assert.equal(runningAnimations,0);await capture('running-reduced');checked('truthful live feedback and no infinite animation under reduced motion');
 await open('rejected');await page.locator('.mission-delivery-group > summary').first().click();assert.match(await page.locator('.mission-delivery').innerText(),/本次验收未通过/);await page.locator('.mission-delivery').scrollIntoViewIfNeeded();await capture('verification-failed');
 await open('completed');assert.match(await page.locator('.mission-detail-progress').innerText(),/2\/2/);await page.getByRole('heading',{name:'交付结果与验收'}).waitFor();await page.locator('.mission-delivery').scrollIntoViewIfNeeded();await capture('completed-delivery');checked('failed verification and overall completion retain evidence entries');
 await tasks();const search=page.getByRole('textbox',{name:'搜索任务'});await search.fill('不存在的任务记录');await page.getByText(/没有匹配|无匹配|未找到/).first().waitFor();await capture('filtered-empty');
 for(const mode of ['empty','error','denied','loading']){
  await page.goto(url+'?mode='+mode);
  if(mode==='loading'){await page.getByText('正在读取任务',{exact:true}).waitFor();await capture('loading');await page.locator('[data-task-id]').first().waitFor();}
  else if(mode==='empty'){await page.getByText(/暂无任务|还没有任务|没有任务/).first().waitFor();await capture('empty');}
  else {await page.getByRole('alert').first().waitFor();await capture(mode);}
 }
 // Snapshot cursor mismatch is tested at the transport boundary as a contract
 // fault. The real store never produced this invalid DTO.
 snapshotMismatch=true;await page.goto(url);await tasks();await page.locator(`[data-task-id="mission:${ids.pause}"]`).getByRole('button',{name:'pause',exact:true}).click();await page.getByRole('alert').first().waitFor();assert.equal(await page.locator('.mission-detail').getAttribute('data-snapshot-cursor'),null);assert.equal(await page.getByRole('button',{name:'续跑未完成任务'}).count(),0);await capture('snapshot-mismatch');checked('mismatched snapshot disables restore and budget submission');
 assert.deepEqual(errors,[]);assert.equal(probeErrors,'');
 writeFileSync(resolve(evidence,'browser-result.json'),JSON.stringify({passed:true,assertions,viewportChecks:[1280,640,390,320],zoomState,reduced,pageErrors:errors,classification:{budget:'real browser + actual unchanged SQLite Store via isolated JSON transport',verification:'contract state seed through store API; not a real expert review',errorAndPermission:'transport contract fixtures',dark:'system dark preference tested; product uses existing light tokens'},snapshots:snapshots.length},null,2));
 console.log('MISSION_UI_BROWSER_EVIDENCE='+evidence);
}catch(error){if(page){await page.screenshot({path:resolve(evidence,'failure.png'),fullPage:true}).catch(()=>{});writeFileSync(resolve(evidence,'failure-dom.txt'),await page.locator('body').innerText().catch(()=>''));}writeFileSync(resolve(evidence,'failure.txt'),String(error.stack||error));console.error('MISSION_UI_BROWSER_EVIDENCE='+evidence);throw error;}
finally{if(browser)await browser.close();await server.close();processProbe.stdin.end();}
