// Replay recorded backend snapshots against the production detail component.
// No missions, task states, events, activities or progress values are generated.
// MISSION_UI_SNAPSHOTS accepts a MissionSnapshot JSON, an array, or a recorded
// interface.jsonl containing {response:{ok:true,value:MissionSnapshot}} entries.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {readFileSync,writeFileSync,mkdirSync} from 'node:fs';
import {createRequire} from 'node:module';
import {dirname,resolve} from 'node:path';
import {fileURLToPath,pathToFileURL} from 'node:url';
import {createServer} from 'vite';
import react from '@vitejs/plugin-react';

assert.ok(process.env.MISSION_UI_SNAPSHOTS,'Set MISSION_UI_SNAPSHOTS to recorded backend snapshots; this check never invents data.');
const input=resolve(process.env.MISSION_UI_SNAPSHOTS);
const raw=readFileSync(input,'utf8');
let records;
try {const value=JSON.parse(raw);records=Array.isArray(value)?value:[value];}
catch {records=raw.trim().split(/\r?\n/).map(line=>JSON.parse(line));}
const seen=new Set();
const samples=records.flatMap(record=>{
  const snapshot=record.snapshot || (record.response?.ok?record.response.value:null) || record;
  if(!snapshot?.mission || !Array.isArray(snapshot.events))return [];
  const activities=record.activities || [];
  const key=JSON.stringify({snapshot,activities});if(seen.has(key))return [];seen.add(key);
  return [{snapshot,activities,source:record.source || 'recorded snapshot (activity provenance unspecified)'}];
});
assert.ok(samples.length,'No recorded MissionSnapshot was found in the input.');
const root=resolve(dirname(fileURLToPath(import.meta.url)),'..');
const require=createRequire(import.meta.url);
const {chromium}=process.env.PLAYWRIGHT_MODULE?await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE).href):require('playwright');
const entry=`
import React from 'react';
import {createRoot} from 'react-dom/client';
import {CollaborationTaskDetail} from '/src/pages/MissionTeamPanel';
import '/styles.css';
const samples=await fetch('/__mission-execution-data').then(response=>response.json());
const sample=samples[Number(new URLSearchParams(location.search).get('sample'))||0];
window.executionMutations=[];
window.__TAURI_INTERNALS__={invoke:async(command)=>{
 if(command==='get_mission_snapshot')return sample.snapshot;
 window.executionMutations.push(command);throw new Error('Unexpected command during snapshot replay: '+command);
}};
createRoot(document.getElementById('root')).render(<CollaborationTaskDetail missionId={sample.snapshot.mission.mission_id} relatedActivities={sample.activities} onBack={()=>{}} onRefresh={async()=>{}}/>);
`;
const virtualPath=resolve(root,'__mission-execution-entry.tsx').replaceAll('\\','/');
const server=await createServer({root,configFile:false,plugins:[react(),{
 name:'mission-execution-snapshot-replay',
 resolveId(id){if(id==='/__mission-execution-entry.tsx'||id===virtualPath)return virtualPath;},
 load(id){if(id===virtualPath)return entry;},
 configureServer(server){
  server.middlewares.use('/__mission-execution-data',(_req,res)=>{res.setHeader('Content-Type','application/json');res.end(JSON.stringify(samples));});
  server.middlewares.use('/__mission-execution',async(req,res,next)=>{
   if(req.url?.split('?')[0] && req.url.split('?')[0]!=='/')return next();
   const html=await server.transformIndexHtml('/__mission-execution','<!doctype html><html lang="zh"><body><div id="root"></div><script type="module" src="/__mission-execution-entry.tsx"></script></body></html>');
   res.setHeader('Content-Type','text/html');res.end(html);
  });
 }
}],server:{host:'127.0.0.1',port:0}});
const evidence=process.env.MISSION_UI_EVIDENCE?resolve(process.env.MISSION_UI_EVIDENCE):null;
if(evidence)mkdirSync(evidence,{recursive:true});
const report={source:input,source_sha256:createHash('sha256').update(raw).digest('hex'),
 semantics:{task_running:'Store assigned/occupied slots; --require-parallel checks lane data only',runtime_live:'Only current mission/task/attempt activity with status running; never inferred from Store Running',verification_live:'Distinct current verification run, exact ownership and associated wait/plan gate'},
 samples:[],pageErrors:[],coverage:{max_task_running:0,max_runtime_live:0,runtime_waiting:false,runtime_queued:false,missing_activity:false,waiting:false,blocked:false,ready:false,awaiting_verification:false,completed:false,scheduling:false,scheduling_wait:false,workspace_prepared:false,candidate_integrated:false,serial_fallback:false}};
let browser;
try {
 await server.listen();
 browser=await chromium.launch({headless:true,...(process.env.CHROMIUM_EXECUTABLE?{executablePath:process.env.CHROMIUM_EXECUTABLE}:{})});
 const page=await browser.newPage({viewport:{width:1280,height:900}});
 page.on('pageerror',error=>report.pageErrors.push(error.message));
 const address=server.httpServer.address();
 for(const [index,sample] of samples.entries()){
  const {mission,events}=sample.snapshot;
  const taskRunning=mission.tasks.filter(task=>task.status==='running').length;
  const active=['active','waiting'].includes(mission.status);
  const currentActivity=task=>active&&task.status==='running'&&task.attempt_run_id?sample.activities.find(item=>item.mission_id===mission.mission_id&&item.mission_task_id===task.spec.id&&item.workflow_run_id===task.attempt_run_id):undefined;
  const running=mission.tasks.filter(task=>currentActivity(task)?.status==='running').length;
  const matchingWait=mission.effective_wait?.contract_version==='mission.wait.v1'&&mission.plan_sha256&&mission.effective_wait.plan_sha256===mission.plan_sha256&&mission.confirmation?.plan_sha256===mission.plan_sha256&&Number.isSafeInteger(mission.effective_wait.cursor)&&mission.effective_wait.cursor>=0;
  const verificationIsLive=task=>Boolean(matchingWait&&active&&task.status==='awaiting_verification'&&task.attempt_run_id&&task.verification_run_id&&task.attempt_run_id!==task.verification_run_id&&sample.activities.some(item=>item.status==='running'&&item.mission_id===mission.mission_id&&item.mission_task_id===task.spec.id&&item.workflow_run_id===task.verification_run_id));
  const verificationLive=mission.tasks.filter(verificationIsLive).length;
  report.coverage.max_task_running=Math.max(report.coverage.max_task_running,taskRunning);
  report.coverage.max_runtime_live=Math.max(report.coverage.max_runtime_live,running);
  report.coverage.runtime_waiting ||= mission.tasks.some(task=>currentActivity(task)?.status==='waiting');
  report.coverage.runtime_queued ||= mission.tasks.some(task=>currentActivity(task)?.status==='queued');
  report.coverage.missing_activity ||= mission.tasks.some(task=>task.status==='running'&&!currentActivity(task));
  report.coverage.waiting ||= mission.status==='waiting';
  report.coverage.scheduling ||= Boolean(mission.scheduling);
  for(const state of ['blocked','ready','awaiting_verification','completed'])report.coverage[state] ||= mission.tasks.some(task=>task.status===state);
  report.coverage.workspace_prepared ||= events.some(event=>event.event_type.replaceAll('.','_')==='task_workspace_prepared');
  report.coverage.candidate_integrated ||= events.some(event=>event.event_type.replaceAll('.','_')==='task_candidate_integrated');
  report.coverage.scheduling_wait ||= events.some(event=>event.event_type.replaceAll('.','_')==='parallel_wave_waiting');
  report.coverage.serial_fallback ||= events.some(event=>event.event_type.includes('serial_fallback')||event.payload?.serial_fallbacks?.length||event.payload?.budget_serial===true);
  await page.goto(`http://127.0.0.1:${address.port}/__mission-execution?sample=${index}`);
  const overview=page.getByRole('region',{name:'执行现场'});
  await overview.waitFor();
  assert.equal(await overview.getByLabel('同时执行任务数').innerText(),`${running} 项任务同时执行`);
  assert.equal(await overview.getByLabel('已派发占用任务数').innerText(),`${taskRunning} 项已派发占用`);
  assert.ok((await page.locator('.mission-detail-progress').innerText()).includes(`${running} 项任务同时执行 · ${taskRunning} 项已派发占用`));
  assert.equal(await overview.locator('[data-execution-task-id]').count(),mission.tasks.length);
  // No run IDs, queued verifications, preparation/admission events or configured
  // capacity can increase the running task counter.
  for(const task of mission.tasks){
   const row=overview.locator(`[data-execution-task-id="${task.spec.id}"]`);
   assert.equal(await row.getAttribute('data-task-status'),task.status);
   for(const label of ['责任者','等待原因','最近活动','下一步'])assert.equal(await row.locator('dt').filter({hasText:label}).count(),1);
   const text=await row.innerText();
   assert.ok(text.includes(task.spec.title));
   assert.equal(await row.locator('dd').filter({hasText:/^$/}).count(),0);
   const latest=events.filter(event=>event.task_id===task.spec.id).sort((a,b)=>b.sequence-a.sequence)[0];
   if(latest?.summary)assert.ok(text.includes(latest.summary),'Show the recorded task activity, including the actual summary.');
   else if(!latest)assert.ok(text.includes('尚无任务活动记录'));
   const live=currentActivity(task)?.status==='running'||verificationIsLive(task);
   assert.equal(await row.getAttribute('data-running'),String(live));
   assert.equal(await row.locator('.busy-indicator').count(),live?1:0,'Only exact current Running ledger activity may animate.');
   if(task.status==='running'){
    const status=currentActivity(task)?.status;
    assert.equal(await row.locator('.mission-task-status').innerText(),status==='running'?'执行中':status==='waiting'?'执行等待中':'已派发，等待运行状态');
    if(status==='waiting')assert.ok(text.includes('执行器等待恢复或外部条件'));
    if(status==='queued')assert.ok(text.includes('执行器排队中'));
   }
  }
  if(mission.status==='waiting')assert.ok((await overview.innerText()).includes('等待原因：'));
  if(matchingWait&&mission.status==='waiting'&&mission.effective_wait.budget_reason)assert.ok((await overview.innerText()).includes(mission.effective_wait.budget_reason));
  assert.equal(await page.locator('.mission-stage-rail.is-live').count(),running||verificationLive?1:0);
  assert.equal(await page.locator('.mission-current-action .busy-indicator').count(),running||verificationLive?1:0);
  if(!running&&!verificationLive){
   assert.equal(await page.locator('.mission-stage-rail.is-live,.mission-stage-rail .is-running,.mission-current-action .busy-indicator').count(),0,'No execution animation without running work.');
  }
  if(mission.scheduling)assert.ok((await overview.innerText()).includes(`配置并发上限 ${mission.scheduling.max_active_tasks}`));
  const decision=events.filter(event=>event.mission_id===mission.mission_id&&event.payload.plan_sha256===mission.plan_sha256&&['parallel_wave_admitted','parallel_wave_waiting'].includes(event.event_type.replaceAll('.','_'))).sort((a,b)=>b.sequence-a.sequence)[0];
  for(const wait of decision?.payload.scheduling?.waiting||[]){
   const task=mission.tasks.find(task=>task.spec.id===wait.task_id);if(!task||!['ready','blocked'].includes(task.status))continue;
   const row=overview.locator(`[data-execution-task-id="${task.spec.id}"]`);const reason=wait.reason;
   if(['dependencies_not_verified','max_active_tasks','executor_capacity','resource_busy'].includes(reason?.code))assert.ok((await row.innerText()).includes('最近调度记录：'));
   if(reason?.code==='resource_busy'&&reason.conflict?.name)assert.ok((await row.innerText()).includes(reason.conflict.name));
  }
  const serialEvents=events.filter(event=>(!event.payload?.plan_sha256||event.payload.plan_sha256===mission.plan_sha256)&&(event.event_type.replaceAll('.','_').includes('serial_fallback')||event.payload?.serial_fallbacks?.length||event.payload?.budget_serial===true));
  if(serialEvents.length){
   const fallback=serialEvents.sort((a,b)=>b.sequence-a.sequence)[0];
   const text=await overview.innerText();assert.ok(text.includes('最近串行降级记录：'));
   for(const value of fallback.payload?.serial_fallbacks||[])if(value.reason)assert.ok(text.includes(value.reason));
  }
  if(mission.scheduling&&(Object.keys(mission.scheduling.executor_limits||{}).length||Object.keys(mission.scheduling.task_resources||{}).length)){
   await overview.getByText('并发与资源配置',{exact:true}).click();
   for(const limit of Object.values(mission.scheduling.executor_limits||{}))assert.ok((await overview.innerText()).includes(`最多 ${limit} 项`));
   for(const resources of Object.values(mission.scheduling.task_resources||{}))for(const resource of resources)assert.ok((await overview.innerText()).includes(resource));
  }
  assert.equal(await overview.getByRole('progressbar').count(),0,'The task overview must not invent percentage progress.');
  await page.setViewportSize({width:390,height:844});
  assert.equal(await overview.evaluate(el=>el.scrollWidth<=el.clientWidth+1),true,'Task facts fit the mobile viewport.');
  if(evidence&&(index<3||running>0||mission.scheduling))await page.screenshot({path:resolve(evidence,`execution-${index}-mobile.png`),fullPage:true});
  await page.setViewportSize({width:1280,height:900});
  await page.getByRole('tab',{name:'执行计划',exact:true}).click();
  for(const task of mission.tasks.filter(task=>task.status==='running')){
   const status=currentActivity(task)?.status;
   assert.equal(await page.locator(`[data-mission-task-id="${task.spec.id}"] .mission-task-status`).innerText(),status==='running'?'执行中':status==='waiting'?'执行等待中':'已派发，等待运行状态');
  }
  await page.getByRole('tab',{name:'活动记录',exact:true}).click();
  const timeline=page.getByRole('tabpanel',{name:'活动记录',exact:true});
  if(events.some(event=>event.event_type.replaceAll('.','_')==='task_workspace_prepared'))assert.ok((await timeline.innerText()).includes('隔离工作区已准备'));
  if(events.some(event=>event.event_type.replaceAll('.','_')==='task_candidate_integrated'))assert.ok((await timeline.innerText()).includes('交付候选已整合，待检查与核验'));
  if(events.some(event=>event.event_type.replaceAll('.','_')==='parallel_wave_waiting'))assert.ok((await timeline.innerText()).includes('调度等待依赖、名额或资源释放'));
  await page.getByRole('tab',{name:'概览',exact:true}).click();
  assert.deepEqual(await page.evaluate(()=>window.executionMutations),[]);
  report.samples.push({index,source:sample.source,mission_id:mission.mission_id,status:mission.status,task_running:taskRunning,runtime_live:running,verification_live:verificationLive,activities:sample.activities.length,tasks:mission.tasks.length,cursor:sample.snapshot.cursor});
 }
 assert.deepEqual(report.pageErrors,[]);
 if(process.argv.includes('--require-parallel'))assert.ok(report.coverage.max_task_running>=2,'No recorded snapshot has two Store Running tasks for parallel lane coverage; this flag does not certify runtime-live execution.');
 report.passed=true;
 console.log(JSON.stringify(report,null,2));
}catch(error){report.passed=false;report.error=String(error);throw error;}
finally {if(evidence)writeFileSync(resolve(evidence,'execution-browser-result.json'),JSON.stringify(report,null,2));await browser?.close();await server.close();}
