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
const fixture = `
import React from 'react';
import {createRoot} from 'react-dom/client';
import {CollaborationTaskDetail} from '/src/pages/MissionTeamPanel';
import {agentApi} from '/src/services/agentApi';
import '/styles.css';
let mission={schema_version:'mission.v1',mission_id:'runtime_fixture',title:'命令环境恢复测试',objective:'验证明确的恢复选择',status:'waiting',workspace:{root:'F:/fixture/workspace',read_scope:['.'],write_scope:['src']},acceptance_criteria:[],plan_revision:1,plan_sha256:'plan-one',plan_summary:'',required_acceptance:[],proposed_tasks:[],budget:null,usage:null,handoff:null,workflow:null,assignment:null,plan_mode:'generated',confirmation:null,tasks:[],created_at:'2026-11-11T00:00:00Z',updated_at:'2026-11-11T00:00:00Z'};
let events=[{sequence:1,event_type:'runtime_workspace_blocked',task_id:'',summary:'命令环境待处理，专家尚未开始',payload:{plan_sha256:'plan-one'},occurred_at:'2026-11-11T00:00:00Z'}];
window.fixtureActions=[];
agentApi.missionSnapshot=async()=>({mission:{...mission},events,cursor:events.length});
agentApi.checkRuntimeWorkspace=async(workspace)=>{window.fixtureActions.push({action:'check'});return {workspace,status:'permission_missing',fingerprint:'fresh-fingerprint',repairable:true,message:'当前用户缺少配置沙箱标签的权限',shell_verified:false};};
agentApi.resolveRuntimeWorkspace=async(input)=>{window.fixtureActions.push(input);if(input.action==='repair')throw new Error(JSON.stringify({message:'管道自检未通过，可仅用文件工具继续'}));mission={...mission,status:'active'};events=[...events,{sequence:2,event_type:'runtime_workspace_resolved',payload:{plan_sha256:'plan-one',mode:'files_only'},occurred_at:'2026-11-11T00:01:00Z'}];return {resolved:true,mission};};
agentApi.controlMission=async()=>{throw new Error('Generic resume must never be called');};
createRoot(document.getElementById('root')).render(<CollaborationTaskDetail missionId='runtime_fixture' onBack={()=>{}} onRefresh={async()=>{}}/>);
`;
const virtualPath = resolve(root,'__runtime-workspace-fixture.tsx').replaceAll('\\','/');
const server = await createServer({root, configFile:false, plugins:[react(),{
  name:'runtime-workspace-fixture',
  resolveId(id){if(id==='/__runtime-workspace-fixture.tsx' || id===virtualPath)return virtualPath;},
  load(id){if(id===virtualPath)return fixture;},
  configureServer(server){server.middlewares.use('/__runtime-workspace-fixture',async(req,res,next)=>{
    if(req.url && req.url!=='/' && req.url!=='')return next();
    const html=await server.transformIndexHtml('/__runtime-workspace-fixture','<!doctype html><html lang="zh"><body><div id="root"></div><script type="module" src="/__runtime-workspace-fixture.tsx"></script></body></html>');
    res.setHeader('Content-Type','text/html');res.end(html);
  });}
}], server:{host:'127.0.0.1',port:0}});
await server.listen();
const browser=await chromium.launch({headless:true, ...(process.env.CHROMIUM_EXECUTABLE ? {executablePath:process.env.CHROMIUM_EXECUTABLE} : {})});
const errors=[];
try {
 const page=await browser.newPage({viewport:{width:1280,height:900}});
 page.on('pageerror',error=>errors.push(error.message));
 await page.goto('http://127.0.0.1:'+server.httpServer.address().port+'/__runtime-workspace-fixture');
 await page.getByRole('button',{name:'处理命令环境',exact:true}).waitFor();
 assert.equal(await page.getByRole('button',{name:'恢复任务',exact:true}).count(),0);
 await page.getByRole('button',{name:'处理命令环境',exact:true}).click();
 const dialog=page.getByRole('dialog');
 await dialog.getByRole('button',{name:'授予完全访问并继续'}).waitFor();
 assert.equal(await dialog.evaluate(el=>el.matches(':modal')),true);
 // Closing the dialog cannot grant permissions or resume execution.
 await page.keyboard.press('Escape');
 assert.equal(await dialog.count(),0);
 assert.deepEqual(await page.evaluate(()=>window.fixtureActions.map(a=>a.action)),['check']);
 await page.getByRole('button',{name:'处理命令环境',exact:true}).click();
 await dialog.getByText('保留工作区隔离（高级）',{exact:true}).click();
 await dialog.getByRole('button',{name:'授权此目录并继续'}).click();
 await dialog.getByRole('alert').waitFor();
 assert.equal(await dialog.count(),1);
 assert.equal(await page.locator('.mission-detail-head').innerText().then(s=>s.includes('命令环境待处理')),true);
 await page.setViewportSize({width:390,height:844});
 assert.equal(await dialog.evaluate(el=>el.scrollWidth<=el.clientWidth+1),true);
 await page.screenshot({path:resolve(evidence,'runtime-workspace-mobile.png')});
 await page.setViewportSize({width:1280,height:900});
 await page.screenshot({path:resolve(evidence,'runtime-workspace-desktop.png')});
 await dialog.getByRole('button',{name:'仅用文件工具继续'}).click();
 await page.waitForFunction(()=>!document.querySelector('dialog'));
 const resolution=await page.evaluate(()=>window.fixtureActions.find(a=>a.action==='files_only'));
 assert.equal(resolution.mission_id,'runtime_fixture');
 assert.equal(resolution.expected_plan_sha256,'plan-one');
 assert.equal(resolution.expected_fingerprint,'fresh-fingerprint');
 const fullPage=await browser.newPage();
 await fullPage.goto('http://127.0.0.1:'+server.httpServer.address().port+'/__runtime-workspace-fixture');
 await fullPage.getByRole('button',{name:'处理命令环境',exact:true}).click();
 await fullPage.getByRole('button',{name:'授予完全访问并继续'}).click();
 await fullPage.waitForFunction(()=>!document.querySelector('dialog'));
 const actions=await fullPage.evaluate(()=>window.fixtureActions);
 assert.equal(actions.filter(action=>action.action==='full_access').length,1);
 assert.equal(actions.some(action=>action.action==='repair'),false);
 assert.equal(actions.find(action=>action.action==='full_access').expected_plan_sha256,'plan-one');
 assert.deepEqual(errors,[]);
 writeFileSync(resolve(evidence,'runtime-workspace-browser.json'),JSON.stringify({passed:true,checks:['modal','cancel-no-mutation','failure-stays-waiting','mobile-overflow','fresh-fingerprint','files-only-resumes']},null,2));
 console.log('runtime workspace browser checks passed');
} finally {await browser.close();await server.close();}
