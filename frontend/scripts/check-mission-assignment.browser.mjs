import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, resolve } from 'node:path';
import { mkdirSync, writeFileSync } from 'node:fs';
import { createServer } from 'vite';
import react from '@vitejs/plugin-react';
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const evidence = process.env.MISSION_UI_EVIDENCE || resolve(root, '../target/mission-assignment-ui');
mkdirSync(evidence, {recursive:true});
const require = createRequire(import.meta.url);
const { chromium } = process.env.PLAYWRIGHT_MODULE
  ? await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE).href)
  : require('playwright');
// This dev-only virtual module mounts the production TaskCenter and API adapter.
// All mutation endpoints remain in memory. Nothing is written to an Agent store.
const fixture = `
import React, {useState} from 'react';
import {createRoot} from 'react-dom/client';
import {TaskCenterPage} from '/src/pages/TaskCenterPage';
import {agentApi} from '/src/services/agentApi';
import '/styles.css';
const pin={expert_id:'com.himind.expert.reviewer',expert_version:'1.0.0',expert_digest:'sha256:review'};
const model=(service,name)=>({alias:'himind:'+service+':'+name,label:name,service_id:service,service_label:service});
const role={role_id:'reviewer',label:'审查',brief:'依据真实证据审查',recommended_executor:'himind.builtin',recommended_service_id:'svc-a',recommended_model:'himind:svc-a:one',model_override_supported:true,availability:'available',unavailable_reason:''};
const options={workflow_id:'com.himind.workflow.test',workflow_version:'1.0.0',workflow_sha256:'digest',source:'workflow',roles:[role],
 experts:[{id:pin.expert_id,version:pin.expert_version,digest:pin.expert_digest,name:'审查专家'}],
 executors:[{executor:'himind.builtin',label:'HiMind AI',available:true,reason:'',models:[model('svc-a','one'),model('svc-a','two'),model('svc-b','three')]},{executor:'personal.codex',label:'Codex CLI',available:true,reason:'',models:[]},{executor:'personal.github-copilot',label:'Copilot',available:false,reason:'未安装',models:[]}]};
let mission={schema_version:'mission.v1',mission_id:'mission_ui_fixture',title:'浏览器测试协作任务',objective:'验证专家指派',status:'awaiting_assignment',workspace:{root:'F:/fixture',read_scope:['.'],write_scope:[]},acceptance_criteria:[],plan_revision:0,plan_sha256:'',plan_summary:'',required_acceptance:[],proposed_tasks:[],budget:null,usage:null,handoff:null,workflow:{workflow_id:options.workflow_id,workflow_version:'1.0.0',workflow_sha256:'digest',entrypoint:''},assignment:null,plan_mode:'generated',confirmation:null,tasks:[],created_at:'2026-11-11T00:00:00Z',updated_at:'2026-11-11T00:00:00Z'};
window.fixtureActions=[];
agentApi.listMissions=async()=>[mission];
agentApi.missionSnapshot=async()=>({mission:{...mission},events:[],cursor:0});
agentApi.missionAssignmentOptions=async()=>options;
agentApi.workflowCenter=async()=>({workflows:[{enabled:true,development:false,package_digest:'digest',package:{id:options.workflow_id,version:'1.0.0',name:'固定协作流程',roles:{reviewer:{}},steps:[]}}]});
agentApi.assignMissionRoles=async(id,input)=>{window.fixtureActions.push('assignment');mission={...mission,status:'planned',assignment:{...input,applied_at:'2026-11-11'}};return mission;};
agentApi.createMissionHandoff=async(input)=>{window.fixtureActions.push('create');mission={...mission,mission_id:'mission_created',title:input.title,status:'awaiting_assignment',assignment:null,workflow:input.workflow};return mission;};
for(const name of ['confirmMissionPlan','dispatchNextMissionTask','controlMission','generateMissionPlan'])agentApi[name]=async()=>{throw new Error('Forbidden in UI fixture: '+name)};
function App(){const [id,setId]=useState('mission_ui_fixture');const [revision,setRevision]=useState(1);window.repeatMissionLink=()=>{setId(mission.mission_id);setRevision(value=>value+1)};return <TaskCenterPage initialMissionId={id} missionNavigationRevision={revision} currentTask={null} dashboardEnabled={false} onLoadTaskHistory={async()=>[]} onLoadLocalActivity={async()=>[]} onOpenDashboard={()=>{}}/>;}
createRoot(document.getElementById('root')).render(<React.StrictMode><App/></React.StrictMode>);
`;
const virtualPath = resolve(root,'__mission-assignment-fixture.tsx').replaceAll('\\','/');
const server = await createServer({root, configFile:false, plugins:[react(),{
  name:'mission-assignment-fixture',
  resolveId(id){if(id==='/__mission-assignment-fixture.tsx' || id===virtualPath)return virtualPath;},
  load(id){if(id===virtualPath)return fixture;},
  configureServer(server){server.middlewares.use('/__mission-assignment-fixture',async(req,res,next)=>{
    if(req.url && req.url!=='/' && req.url!=='')return next();
    const html=await server.transformIndexHtml('/__mission-assignment-fixture','<!doctype html><html lang="zh"><body><div id="root"></div><script type="module" src="/__mission-assignment-fixture.tsx"></script></body></html>');
    res.setHeader('Content-Type','text/html');res.end(html);
  });}
}], server:{host:'127.0.0.1',port:0}});
await server.listen();
const browser=await chromium.launch({headless:true, ...(process.env.CHROMIUM_EXECUTABLE ? {executablePath:process.env.CHROMIUM_EXECUTABLE} : {})});
const errors=[];
try {
 const page=await browser.newPage({viewport:{width:1280,height:900}});
 page.on('pageerror',error=>errors.push(error.message));
 const address=server.httpServer.address();
 await page.goto('http://127.0.0.1:'+address.port+'/__mission-assignment-fixture');
 const dialog=page.getByRole('dialog');
 await dialog.waitFor();
 assert.equal(await dialog.evaluate(el=>el.matches(':modal')),true);
 await page.getByLabel('审查专家').selectOption({label:'审查专家 · 1.0.0'});
 const expert=await page.getByLabel('审查专家').inputValue();
 await page.getByLabel('审查服务').selectOption('svc-b');
 assert.equal(await page.getByLabel('审查模型').inputValue(),'himind:svc-b:three');
 await page.getByLabel('审查执行器').selectOption('personal.codex');
 assert.equal(await page.getByLabel('审查服务').isDisabled(),true);
 assert.equal(await page.getByLabel('审查模型').inputValue(),'');
 await page.getByRole('button',{name:'使用推荐服务与模型'}).click();
 assert.equal(await page.getByLabel('审查执行器').inputValue(),'himind.builtin');
 assert.equal(await page.getByLabel('审查专家').inputValue(),expert);
 assert.equal(await page.getByLabel('审查模型').inputValue(),'himind:svc-a:one');
 await page.screenshot({path:resolve(evidence,'assignment-modal-desktop.png')});
 await page.setViewportSize({width:390,height:844});
 await page.screenshot({path:resolve(evidence,'assignment-modal-mobile.png')});
 assert.equal(await dialog.evaluate(el=>el.scrollWidth<=el.clientWidth+1),true);
 await page.setViewportSize({width:1280,height:900});
 // Tab may focus the dialog or browser chrome on wrap, but never a background control.
 for(let index=0;index<16;index++){
  await page.keyboard.press('Tab');
  assert.equal(await page.evaluate(()=>document.activeElement===document.body || !!document.activeElement.closest('dialog')),true);
 }
 await page.keyboard.press('Escape');
 assert.equal(await page.getByRole('dialog').count(),0);
 const trigger=page.getByRole('button',{name:'选择协作专家'});
 await trigger.click();await dialog.waitFor();
 await dialog.getByRole('button',{name:'关闭',exact:true}).click();
 assert.equal(await trigger.evaluate(el=>el===document.activeElement),true);
 await page.evaluate(()=>window.repeatMissionLink());await dialog.waitFor();
 assert.equal(await dialog.evaluate(el=>el.matches(':modal')),true);
 await page.keyboard.press('Escape');
 assert.deepEqual(await page.evaluate(()=>window.fixtureActions),[]);
 await trigger.click();
 await page.getByLabel('审查专家').selectOption(expert);
 await page.getByRole('button',{name:'应用角色指派'}).click();
 await dialog.waitFor({state:'detached'});
 assert.equal(await page.getByText('com.himind.expert.reviewer · 1.0.0').count(),1);
 assert.deepEqual(await page.evaluate(()=>window.fixtureActions),['assignment']);
 await page.getByRole('button',{name:'返回我的任务'}).click();
 await page.getByRole('button',{name:'新建任务',exact:true}).click();
 await page.getByRole('button',{name:'新建协作任务'}).click();await dialog.waitFor();
 assert.equal(await dialog.evaluate(el=>el.matches(':modal')),true);
 assert.equal(await page.getByRole('button',{name:'创建任务',exact:true}).isDisabled(),true);
 await page.getByLabel('协作工作流').selectOption('com.himind.workflow.test');
 await page.getByLabel('任务标题').fill('浏览器内存草稿');
 await page.getByLabel('工作区根目录').fill('F:/fixture');
 await page.screenshot({path:resolve(evidence,'create-modal-desktop.png')});
 await page.setViewportSize({width:390,height:844});
 await page.screenshot({path:resolve(evidence,'create-modal-mobile.png')});
 assert.equal(await dialog.evaluate(el=>el.scrollWidth<=el.clientWidth+1),true);
 await page.keyboard.press('Escape');
 assert.equal(await page.getByRole('dialog').count(),0);
 assert.deepEqual(await page.evaluate(()=>window.fixtureActions),['assignment']);
 assert.deepEqual(errors,[]);
 writeFileSync(resolve(evidence,'browser-ui-result.json'),JSON.stringify({passed:true,checks:['native-modal','directory-cascade','expert-preserved-on-recommendation','tab-background-inert','escape','focus-return','same-mission-reopen','summary','workflow-required','desktop-mobile-no-overflow','no-confirm-authorize-or-dispatch'],pageErrors:errors},null,2));
 console.log('Mission browser UI behavior passed');
} finally {await browser.close();await server.close();}
