// Directory behavior checks; add --browser to mount the production TaskCenter
// and API adapter in Chromium. Mutations stay in memory, without an Agent store.
import { strict as assert } from 'node:assert';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { createRequire } from 'node:module';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { assignmentProblem, bindExpert, changeExecutor, changeService, hasMissingTaskAssignment, initialAssignmentRoles, initialTaskRoles, recommendedRole, serviceOptions } from '../src/pages/missionAssignment.ts';
import type { ExpertSummary, MissionAssignmentOptions, MissionExecutionOption, MissionRoleAssignment, MissionView } from '../src/services/agentApi';

const expert = { id: 'com.himind.expert.test', version: '1.0.0', digest: 'sha256:pin' } as ExpertSummary;
const executor: MissionExecutionOption = {
  executor: 'himind.builtin', label: 'HiMind AI', available: true, reason: '',
  models: [
    {alias:'himind:a:one', label:'one', service_id:'a', service_label:'A'},
    {alias:'himind:a:two', label:'two', service_id:'a', service_label:'A'},
    {alias:'himind:b:one', label:'one', service_id:'b', service_label:'B'},
  ],
};
const client: MissionExecutionOption = {executor:'personal.codex', label:'Codex', available:true, reason:'', models:[]};
const options = {experts:[expert], executors:[executor, client]} as MissionAssignmentOptions;
const role: MissionRoleAssignment = bindExpert({role_id:'reviewer', label:'Review', executor:'himind.builtin', service_id:'a', model:'himind:a:two'}, expert);
assert.equal(assignmentProblem(role, options), '');
assert.deepEqual(serviceOptions(executor).map(item => item.id), ['a', 'b']);
const switched = changeService(role, executor, 'b');
assert.equal(switched.model, 'himind:b:one');
assert.equal(assignmentProblem(switched, options), '');
assert.ok(assignmentProblem({...role, service_id:'b'}, options));
assert.ok(assignmentProblem({...role, expert:null}, options));
assert.ok(assignmentProblem({...role, expert:{...role.expert!, expert_digest:'drift'}}, options));
const onClient = changeExecutor(role, client);
assert.equal(onClient.model, ''); assert.equal(onClient.service_id, '');
assert.deepEqual(onClient.expert, role.expert);
assert.equal(assignmentProblem(onClient, options), '');
assert.ok(assignmentProblem({...role, model:'', service_id:''}, {...options, executors:[{...executor, models:[]}]}));
console.log('mission directory behavior checks passed');

const slot = { role_id:'reviewer', label:'Review', brief:'', recommended_expert:role.expert,
  recommended_executor:executor.executor, recommended_service_id:'a', recommended_model:'himind:a:one',
  model_override_supported:true, availability:'available', unavailable_reason:'' };
const catalog: MissionAssignmentOptions = { ...options, workflow_id:'test', workflow_version:'1.0.0', workflow_sha256:'digest', source:'workflow',
  roles:[slot, {...slot,role_id:'designer',optional:true,recommended_expert:null}] };
assert.deepEqual(recommendedRole(slot,catalog).expert,role.expert);
assert.equal(recommendedRole(slot,{...catalog,experts:[{...expert,digest:'drift'}]}).expert,null,'摘要不匹配不能冒充推荐');
assert.equal(recommendedRole({...slot,recommended_expert:null},catalog).expert,null,'旧工作流未推荐时保留手选');
const mission = {proposed_tasks:[],assignment:null} as unknown as MissionView;
assert.deepEqual(initialAssignmentRoles(mission,catalog).map(value=>value.role_id),['reviewer'],'无推荐可选职责不应阻塞');
const manual = {...onClient,expert:{...role.expert!,expert_digest:'manual-pin'}};
const saved = {...mission,assignment:{roles:[manual],task_roles:[],applied_by:'user',applied_at:'',recommended:false}};
assert.deepEqual(initialAssignmentRoles(saved,catalog)[0],manual,'已有人工绑定优先');
const optionalRecommended = {...catalog,roles:[slot,{...slot,role_id:'designer',optional:true}]};
assert.equal(initialAssignmentRoles(mission,optionalRecommended).length,2);
assert.equal(initialAssignmentRoles(saved,optionalRecommended).length,1,'已保存的可选职责停用不能被推荐重新开启');
const taskMission = {...saved,proposed_tasks:[{id:'task',execution:{role_id:'designer'}}],
  assignment:{...saved.assignment,task_roles:[{task_id:'task',role_id:'reviewer'}]}} as MissionView;
assert.deepEqual(initialTaskRoles(taskMission,[role]),{task:'reviewer'},'保留已有任务映射');
assert.equal(hasMissingTaskAssignment(taskMission,{task:'designer'},[role]),true,'不能引用未启用职责');
assert.equal(hasMissingTaskAssignment(taskMission,{task:'reviewer'},[role]),false);
assert.equal(initialAssignmentRoles({...taskMission,assignment:null},catalog).length,2,'任务引用的可选职责需要指派');
console.log('mission recommendation behavior checks passed');

export async function checkMissionAssignmentBrowser() {
  const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
  const { createServer } = await import('vite');
  const { default: react } = await import('@vitejs/plugin-react');
  const require = createRequire(import.meta.url);
  const { chromium } = process.env.PLAYWRIGHT_MODULE ? await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE).href) : require('playwright');
  const recordedHeader = process.env.MISSION_HEADER_SNAPSHOT
    ? JSON.parse(readFileSync(process.env.MISSION_HEADER_SNAPSHOT, 'utf8')).mission : null;
  // Virtual entry imports production components; this is not a static UI copy.
  const fixture = `
import React,{useState} from 'react';
import {createRoot} from 'react-dom/client';
import {TaskCenterPage} from '/src/pages/TaskCenterPage';
import {agentApi} from '/src/services/agentApi';
import '/styles.css';
const scenario=new URLSearchParams(location.search).get('scenario')||'recommended';
const pin={expert_id:'com.himind.expert.reviewer',expert_version:'1.0.0',expert_digest:'sha256:review'};
const manualPin={expert_id:'com.himind.expert.manual',expert_version:'2.0.0',expert_digest:'sha256:manual'};
const model=(service,name)=>({alias:'himind:'+service+':'+name,label:name,service_id:service,service_label:service});
const role={role_id:'reviewer',label:'审查',brief:'依据真实证据审查',recommended_expert:scenario==='legacy'?null:pin,recommendation_reason:scenario==='legacy'?'':'需求命中审查关键词，匹配审查职责',recommended_executor:'himind.builtin',recommended_service_id:'svc-a',recommended_model:'himind:svc-a:one',model_override_supported:true,availability:'available',unavailable_reason:''};
const optional={...role,role_id:'designer',label:'设计',optional:true,recommended_expert:null,recommendation_reason:'未命中可用设计专家，请按需手动指派'};
const options={workflow_id:'com.himind.workflow.test',workflow_version:'1.0.0',workflow_sha256:'digest',source:'workflow',roles:[role,optional],recommendation_warnings:['推荐基于关键词；缺少设计专业时请手动补充专家。'],
 experts:[{id:pin.expert_id,version:pin.expert_version,digest:'sha256:other',name:'同版本其他摘要'},{id:pin.expert_id,version:pin.expert_version,digest:pin.expert_digest,name:'审查专家'},{id:manualPin.expert_id,version:manualPin.expert_version,digest:manualPin.expert_digest,name:'手选专家'}],
 executors:[{executor:'himind.builtin',label:'HiMind AI',available:true,reason:'',models:[model('svc-a','one'),model('svc-a','two'),model('svc-b','three')]},{executor:'personal.codex',label:'Codex CLI',available:true,reason:'',models:[]},{executor:'personal.github-copilot',label:'Copilot',available:false,reason:'未安装',models:[]}]};
const task={id:'design',title:'设计任务',objective:'完成设计',depends_on:[],read_paths:[],write_paths:[],acceptance_criteria:[],required_checks:[],execution:{role_id:'designer',executor:'himind.builtin',service_id:'svc-a',model:'himind:svc-a:one'},repair_of:''};
let mission={schema_version:'mission.v1',mission_id:'mission_ui_fixture',title:'浏览器测试协作任务',objective:'验证专家指派',status:'awaiting_assignment',workspace:{root:'F:/fixture',permission_mode:'workspace_write',read_scope:['.'],write_scope:[]},acceptance_criteria:[],plan_revision:0,plan_sha256:'',plan_summary:'',required_acceptance:[],proposed_tasks:scenario==='referenced'?[task]:[],budget:null,usage:null,handoff:null,workflow:{workflow_id:options.workflow_id,workflow_version:'1.0.0',workflow_sha256:'digest',entrypoint:''},assignment:scenario==='manual'?{roles:[{role_id:'reviewer',label:'审查',expert:manualPin,executor:'personal.codex',service_id:'',model:''}],task_roles:[],applied_at:'2026-11-16',applied_by:'user',recommended:false}:null,plan_mode:'generated',confirmation:null,tasks:[],created_at:'2026-11-16T00:00:00Z',updated_at:'2026-11-16T00:00:00Z'};
if(scenario.startsWith('header-')){
 const long=scenario.endsWith('-long');
 const recorded=${JSON.stringify(recordedHeader ? {title:recordedHeader.title,objective:recordedHeader.objective} : null)};
 mission={...mission,status:'planned',title:long?'协作任务长标题换行验证'.repeat(6):mission.title,
  objective:long?'完成独立模块并记录验收结果，保留原始测试与工作区约束。'.repeat(8)+'F:/workspace/'+ 'unbroken-path-segment'.repeat(15):mission.objective,
  ...(scenario.endsWith('-recorded')?recorded:{}),
  tasks:scenario.startsWith('header-progress-')?[{spec:task,status:'ready',checks:[],artifacts:[],evidence_refs:[]}]:[]};
}
window.fixtureActions=[];
window.fixtureWorkflowReads=0;
agentApi.listMissions=async()=>[mission];
agentApi.localActivity=async()=>[];
agentApi.missionSnapshot=async()=>({mission:{...mission},events:[],cursor:0});
agentApi.missionAssignmentOptions=async()=>options;
agentApi.workflowCenter=async()=>{window.fixtureWorkflowReads++;if(scenario==='workflow-error')throw new Error('工作流目录暂不可用');return {workflows:[{enabled:true,development:false,package_digest:'digest',package:{id:options.workflow_id,version:'1.0.0',name:'固定协作流程',roles:{reviewer:{}},steps:[]}}]};};
agentApi.assignMissionRoles=async(id,input)=>{window.fixtureActions.push({kind:'assignment',input});mission={...mission,status:'planned',assignment:{...input,applied_at:'2026-11-16'}};return mission;};
const create=async(kind,input)=>{window.fixtureActions.push({kind,input});mission={...mission,mission_id:'mission_created',title:input.title,objective:input.objective,workspace:input.workspace,status:'awaiting_assignment',assignment:null,proposed_tasks:[],workflow:input.workflow||mission.workflow};return mission;};
agentApi.createMissionIntent=async(input)=>{if('workflow' in input||'plan_mode' in input)throw new Error('Intent cannot include workflow or plan_mode');return create('intent',input);};
agentApi.createMissionHandoff=async(input)=>create('handoff',input);
for(const name of ['createMission','confirmMissionPlan','dispatchNextMissionTask','controlMission','generateMissionPlan'])agentApi[name]=async()=>{window.fixtureActions.push({kind:'forbidden:'+name});throw new Error('Forbidden in UI fixture: '+name);};
function App(){const [id,setId]=useState(mission.mission_id);const [revision,setRevision]=useState(1);window.repeatMissionLink=()=>{setId(mission.mission_id);setRevision(value=>value+1)};return <TaskCenterPage initialMissionId={id} missionNavigationRevision={revision} currentTask={null} dashboardEnabled={false} onLoadTaskHistory={async()=>[]} onLoadLocalActivity={async()=>[]} onOpenDashboard={()=>{}}/>;}
createRoot(document.getElementById('root')).render(<React.StrictMode><App/></React.StrictMode>);
`;
  const virtualPath = resolve(root,'__mission-assignment-fixture.tsx').replaceAll('\\','/');
  const server = await createServer({root,configFile:false,plugins:[react(),{
    name:'mission-assignment-fixture',
    resolveId(id){if(id==='/__mission-assignment-fixture.tsx'||id===virtualPath)return virtualPath;},
    load(id){if(id===virtualPath)return fixture;},
    configureServer(server){server.middlewares.use('/__mission-assignment-fixture',async(req,res,next)=>{
      const path=req.url?.split('?')[0]; if(path&&path!=='/')return next();
      const html=await server.transformIndexHtml('/__mission-assignment-fixture','<!doctype html><html lang="zh"><body><div id="root"></div><script type="module" src="/__mission-assignment-fixture.tsx"></script></body></html>');
      res.setHeader('Content-Type','text/html');res.end(html);
    });},
  }],server:{host:'127.0.0.1',port:0}});
  let browser;
  try {
    await server.listen();
    browser=await chromium.launch({headless:true,...(process.env.CHROMIUM_EXECUTABLE?{executablePath:process.env.CHROMIUM_EXECUTABLE}:{})});
    const errors: string[]=[];
    const page=await browser.newPage({viewport:{width:1280,height:900}});
    page.on('pageerror',(error: Error)=>errors.push(error.message));
    const address=server.httpServer!.address();assert.ok(address&&typeof address==='object');
    const url=`http://127.0.0.1:${address.port}/__mission-assignment-fixture`;
    const dialog=page.getByRole('dialog');
    const apply=page.getByRole('button',{name:'应用角色指派'});
    const recommendedKey='com.himind.expert.reviewer@1.0.0:sha256:review';
    const manualKey='com.himind.expert.manual@2.0.0:sha256:manual';
    const actions=()=>page.evaluate(()=>(window as any).fixtureActions);
    const load=async(scenario='recommended')=>{await page.goto(`${url}?scenario=${scenario}`);await dialog.waitFor();await page.getByLabel('审查专家',{exact:true}).waitFor();};
    const openCreate=async()=>{
      await page.keyboard.press('Escape');await page.getByRole('button',{name:'返回我的任务'}).click();
      await page.getByRole('button',{name:'新建任务',exact:true}).click();await page.getByRole('button',{name:'新建协作任务',exact:true}).click();
      await page.getByLabel('需求描述').waitFor();
    };
    const checkWidth=async()=>{await page.setViewportSize({width:390,height:844});assert.equal(await dialog.evaluate((el: HTMLElement)=>el.scrollWidth<=el.clientWidth+1),true);await page.setViewportSize({width:1280,height:900});};

    if(process.argv.includes('--header-layout')){
      const evidence=process.env.MISSION_HEADER_EVIDENCE_DIR;
      if(evidence)mkdirSync(evidence,{recursive:true});
      const samples: any[]=[];
      const failures: string[]=[];
      const sizes=[{width:1280,height:900},{width:900,height:640},{width:721,height:900},{width:720,height:900},{width:390,height:844},{width:320,height:844}];
      const variants=recordedHeader?['short','long','recorded']:['short','long'];
      for(const size of sizes){
        await page.setViewportSize(size);
        for(const state of ['empty','progress'])for(const variant of variants){
          const scenario=`header-${state}-${variant}`;
          await page.goto(`${url}?scenario=${scenario}`);
          const hero=page.locator('.mission-detail-hero');
          await hero.waitFor();
          await page.evaluate(()=>document.fonts.ready);
          const geometry=await hero.evaluate((el: HTMLElement)=>{
            const bounds=(node: Element)=>{const r=node.getBoundingClientRect();return {x:r.x,y:r.y,width:r.width,height:r.height,right:r.right,bottom:r.bottom};};
            const measure=(node: HTMLElement)=>{
              const range=document.createRange();range.selectNodeContents(node);
              const fragments=[...range.getClientRects()].filter(rect=>rect.width>0);
              return {...bounds(node),scrollWidth:node.scrollWidth,clientWidth:node.clientWidth,
                fontSize:getComputedStyle(node).fontSize,lineCount:new Set(fragments.map(rect=>Math.round(rect.y))).size,
                textFits:fragments.every(rect=>rect.x>=node.getBoundingClientRect().x-1&&rect.right<=node.getBoundingClientRect().right+1)};
            };
            const copy=el.firstElementChild as HTMLElement;
            const progress=el.lastElementChild as HTMLElement;
            const empty=progress.textContent==='未记录执行进度';
            return {hero:bounds(el),heroScrollWidth:el.scrollWidth,heroClientWidth:el.clientWidth,
              copy:bounds(copy),title:measure(copy.querySelector('h2')!),objective:measure(copy.querySelector('p')!),
              progress:bounds(progress),progressScrollWidth:progress.scrollWidth,progressClientWidth:progress.clientWidth,
              emptyText:empty?measure(progress.querySelector('span')||progress):null,
              textAlign:getComputedStyle(progress).textAlign,zoom:getComputedStyle(document.documentElement).zoom};
          });
          const check=(passed: boolean,message: string)=>{if(!passed)failures.push(`${size.width}/${scenario}: ${message}`);};
          const stacked=size.width<=720;
          check(geometry.heroScrollWidth<=geometry.heroClientWidth+1,'hero overflows horizontally');
          check(geometry.progressScrollWidth<=geometry.progressClientWidth+1,'progress overflows horizontally');
          check(geometry.title.textFits&&geometry.objective.textFits,'title/objective text leaves its column');
          check(geometry.progress.right<=geometry.hero.right+1,'progress leaves hero');
          check(Math.abs(geometry.progress.width-(stacked?geometry.copy.width:220))<=1,'progress width changes with content');
          check(stacked?geometry.progress.y>=geometry.copy.bottom+17:Math.abs(geometry.progress.y-geometry.copy.y)<=1,'incorrect row/stack placement');
          check(stacked?Math.abs(geometry.progress.x-geometry.copy.x)<=1:geometry.progress.x>=geometry.copy.right+17,'columns overlap or lose gap');
          check(geometry.textAlign===(stacked?'left':'right'),'incorrect progress alignment');
          if(state==='empty')check(geometry.emptyText?.lineCount===1,'empty progress wraps into multiple lines');
          if(variant==='long')check(geometry.title.lineCount>1&&geometry.objective.lineCount>1,'long copy does not wrap');
          samples.push({viewport:size,scenario,...geometry});
          if(evidence&&variant!=='short'&&[1280,900,720,390].includes(size.width))await hero.screenshot({path:resolve(evidence,`${scenario}-${size.width}.png`)});
        }
      }
      assert.deepEqual(await actions(),[],'Header layout checks must not mutate the Agent');
      const sourceHashes=Object.fromEntries(['src/pages/MissionTeamPanel.tsx','styles.css','scripts/check-mission-assignment-ui.mts'].map(path=>[path,createHash('sha256').update(readFileSync(resolve(root,path))).digest('hex')]));
      const report={checkedAt:new Date().toISOString(),command:process.argv,recordedHeaderSource:process.env.MISSION_HEADER_SNAPSHOT||null,
        browserVersion:browser.version(),sourceHashes,pageErrors:errors,failures,samples};
      if(evidence)writeFileSync(resolve(evidence,'header-layout.json'),JSON.stringify(report,null,2)+'\n');
      assert.deepEqual(errors,[]);
      assert.deepEqual(failures,[]);
      console.log(`Mission header layout passed (${samples.length} cases; normal font size; empty/progress; short/long/recorded; 320-1280px)`);
      return;
    }

    await load();
    assert.equal(await dialog.evaluate((el: Element)=>el.matches(':modal')),true);
    assert.equal(await page.getByLabel('审查专家',{exact:true}).inputValue(),recommendedKey,'按精确摘要预选推荐专家');
    assert.equal(await page.getByText('推荐理由：需求命中审查关键词，匹配审查职责').count(),1);
    assert.equal(await page.getByText('推荐基于关键词；缺少设计专业时请手动补充专家。').count(),1);
    const optionalSwitch=page.getByRole('switch',{name:'设计职责'});
    assert.equal(await optionalSwitch.getAttribute('aria-checked'),'false');assert.equal(await apply.isEnabled(),true);
    await optionalSwitch.click();assert.equal(await page.getByLabel('设计专家',{exact:true}).isEnabled(),true);assert.equal(await apply.isDisabled(),true);
    await page.getByLabel('设计专家',{exact:true}).selectOption(manualKey);await optionalSwitch.click();
    await page.getByRole('button',{name:'使用推荐服务与模型'}).click();assert.equal(await optionalSwitch.getAttribute('aria-checked'),'false');
    await optionalSwitch.click();assert.equal(await page.getByLabel('设计专家',{exact:true}).inputValue(),manualKey,'启停保留手选');await optionalSwitch.click();
    await page.getByLabel('审查专家',{exact:true}).selectOption(manualKey);
    await page.getByLabel('审查服务',{exact:true}).selectOption('svc-b');assert.equal(await page.getByLabel('审查模型',{exact:true}).inputValue(),'himind:svc-b:three');
    await page.getByLabel('审查执行器',{exact:true}).selectOption('personal.codex');assert.equal(await page.getByLabel('审查服务',{exact:true}).isDisabled(),true);assert.equal(await page.getByLabel('审查模型',{exact:true}).inputValue(),'');
    await page.getByRole('button',{name:'使用推荐服务与模型'}).click();
    assert.equal(await page.getByLabel('审查专家',{exact:true}).inputValue(),manualKey);assert.equal(await page.getByLabel('审查执行器',{exact:true}).inputValue(),'himind.builtin');assert.equal(await page.getByLabel('审查模型',{exact:true}).inputValue(),'himind:svc-a:one');
    assert.deepEqual(await actions(),[],'配置推荐不会写入或启动');await checkWidth();
    for(let index=0;index<20;index++){await page.keyboard.press('Tab');assert.equal(await page.evaluate(()=>document.activeElement===document.body||!!document.activeElement?.closest('dialog')),true);}
    await page.keyboard.press('Escape');assert.equal(await dialog.count(),0);
    const trigger=page.getByRole('button',{name:'选择协作专家',exact:true});
    await trigger.click();await dialog.waitFor();await dialog.getByRole('button',{name:'关闭',exact:true}).click();assert.equal(await trigger.evaluate((el: Element)=>el===document.activeElement),true);
    await page.evaluate(()=>(window as any).repeatMissionLink());await dialog.waitFor();await apply.click();await dialog.waitFor({state:'detached'});
    const assignmentActions=await actions();assert.deepEqual(assignmentActions.map((action: any)=>action.kind),['assignment']);assert.deepEqual(assignmentActions[0].input.roles.map((value: any)=>value.role_id),['reviewer']);
    assert.equal(await page.getByText('com.himind.expert.reviewer · 1.0.0').count(),1);

    await load('manual');assert.equal(await page.getByLabel('审查专家',{exact:true}).inputValue(),manualKey);assert.equal(await page.getByLabel('审查执行器',{exact:true}).inputValue(),'personal.codex');
    await page.getByRole('button',{name:'使用推荐服务与模型'}).click();assert.equal(await page.getByLabel('审查专家',{exact:true}).inputValue(),manualKey);
    await load('legacy');assert.equal(await page.getByLabel('审查专家',{exact:true}).inputValue(),'');assert.equal(await apply.isDisabled(),true);
    await page.getByLabel('审查专家',{exact:true}).selectOption(manualKey);assert.equal(await apply.isEnabled(),true,'老工作流可以手选');
    await load('referenced');assert.equal(await optionalSwitch.getAttribute('aria-checked'),'true');assert.equal(await apply.isDisabled(),true,'任务引用职责必须有专家');
    await page.getByLabel('设计专家',{exact:true}).selectOption(manualKey);assert.equal(await apply.isEnabled(),true);await optionalSwitch.click();assert.equal(await apply.isDisabled(),true,'不能引用停用职责');
    await page.getByLabel('设计任务承担专家').selectOption('reviewer');assert.equal(await apply.isEnabled(),true);await apply.click();await dialog.waitFor({state:'detached'});
    assert.deepEqual((await actions())[0].input.task_roles,[{task_id:'design',role_id:'reviewer'}]);

    await load();await openCreate();
    const create=page.getByRole('button',{name:'创建草案',exact:true});
    assert.equal(await dialog.evaluate((el: Element)=>el.matches(':modal')),true);
    assert.equal(await page.getByLabel('需求描述').evaluate((el: Element)=>el===el.closest('dialog')?.querySelector('input,textarea,select')),true);
    await page.getByLabel('任务标题').fill('只有标题');await page.getByLabel('工作区根目录').fill('F:/fixture');assert.equal(await create.isDisabled(),true);
    await page.getByLabel('任务标题').fill('');await page.getByLabel('需求描述').fill('审查设置页焦点问题');assert.equal(await create.isEnabled(),true,'无需先选择工作流或标题');
    assert.equal(await page.evaluate(()=>(window as any).fixtureWorkflowReads),0);
    assert.deepEqual(await dialog.getByLabel('执行权限').locator('option').evaluateAll((nodes: HTMLOptionElement[])=>nodes.map(node=>node.value)),['workspace_write','full_access']);
    await dialog.getByLabel('执行权限').selectOption('full_access');await checkWidth();await create.click();
    await page.getByRole('heading',{name:'选择协作专家',exact:true}).waitFor();
    const created=await actions();assert.deepEqual(created.map((action: any)=>action.kind),['intent']);assert.equal(created[0].input.objective,'审查设置页焦点问题');assert.equal(created[0].input.title,'审查设置页焦点问题');assert.equal(created[0].input.workspace.permission_mode,'full_access');
    assert.equal('workflow' in created[0].input,false);assert.equal('plan_mode' in created[0].input,false);assert.equal(created[0].input.idempotency_key,created[0].input.source_reference);
    assert.equal(await page.getByLabel('审查专家',{exact:true}).inputValue(),recommendedKey);

    await load();await openCreate();await page.getByLabel('需求描述').fill('固定流程草案');await page.getByLabel('工作区根目录').fill('F:/fixture');
    await page.getByText('进阶选项：固定工作流与修改范围',{exact:true}).click();await page.getByLabel('固定工作流').selectOption('com.himind.workflow.test');await page.getByLabel('允许修改的范围').fill('frontend/src, docs');await create.click();
    await page.getByRole('heading',{name:'选择协作专家',exact:true}).waitFor();const fixed=await actions();assert.deepEqual(fixed.map((action: any)=>action.kind),['handoff']);assert.equal(fixed[0].input.workflow.workflow_id,'com.himind.workflow.test');assert.equal(fixed[0].input.plan_mode,'generated');assert.deepEqual(fixed[0].input.workspace.write_scope,['frontend/src','docs']);
    await load('workflow-error');await openCreate();await page.getByText('进阶选项：固定工作流与修改范围',{exact:true}).click();await page.getByText(/固定工作流读取失败，可继续自动组队/).waitFor();
    await page.getByLabel('需求描述').fill('目录不可用时仍创建草案');await page.getByLabel('工作区根目录').fill('F:/fixture');await create.click();await page.getByRole('heading',{name:'选择协作专家',exact:true}).waitFor();assert.deepEqual((await actions()).map((action: any)=>action.kind),['intent']);
    assert.deepEqual(errors,[]);
    console.log('Mission browser UI passed (intent / optional fixed workflow / exact recommendation / manual priority / optional roles / warnings / selectors / modal / mobile / no execution)');
  } finally {await browser?.close();await server.close();}
}

if(process.argv.includes('--browser')) await checkMissionAssignmentBrowser();
