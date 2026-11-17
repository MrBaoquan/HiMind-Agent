// In-memory host fixture; renders the production desktop shell and Mission components.
import React, {useState} from 'react';
import {createRoot} from 'react-dom/client';
import {Shell} from '/src/components/Shell';
import {TaskCenterPage} from '/src/pages/TaskCenterPage';
import {agentApi} from '/src/services/agentApi';
import '/styles.css';
const noop=()=>{}, refresh=async()=>{};
const pin={expert_id:'com.himind.expert.software',expert_version:'1.0.0',expert_digest:'sha256:fixture',skill_pins:[]};
const binding={role_id:'software',expert:pin,executor:'personal.codex',service_id:'',model:''};
const makeTask=(id,title,depends_on=[],executor='personal.codex')=>({spec:{id,title,objective:'完成当前已确认的职责，交付可验证结果。',depends_on,read_paths:['src'],write_paths:['src/'+id],acceptance_criteria:['必需检查与独立验收通过'],required_checks:[],execution:{...binding,executor},repair_of:''},status:'running',attempt_run_id:'run-'+id,verification_run_id:'',verification_passed:null,evidence_refs:[]});
let tasks=[makeTask('ui','界面导航与交互反馈',[],'himind.builtin'),makeTask('data','内容数据适配与存储'),makeTask('qa','整合与独立验收',['ui','data'])];tasks[2].status='blocked';
let mission={schema_version:'mission.v1',mission_id:'fixture-workbench',title:'展项体验升级 · 桌面协作工作台',objective:'优化界面与内容读取，交付实际验证成果。',status:'active',workspace:{root:'F:/fixture/exhibit',read_scope:['.'],write_scope:['src'],permission_mode:'full_access',adjustment_mode:'automatic'},acceptance_criteria:['界面和内容读取通过验收'],plan_revision:3,plan_sha256:'plan3',plan_summary:'两个独立分支并行，依赖满足后整合验收。',required_acceptance:[],proposed_tasks:tasks.map(t=>t.spec),budget:null,usage:null,handoff:{source_client:'codex',context_summary:''},workflow:null,assignment:{roles:[{...binding,label:'软件工程专家'}],task_roles:[],applied_by:'fixture',applied_at:new Date().toISOString(),recommended:false},confirmation:{plan_sha256:'plan3',selected_task_ids:['ui','data','qa'],approved_paths:['src'],approved_absolute_read_paths:[],approved_absolute_write_paths:[],confirmed_by:'fixture',confirmed_at:new Date().toISOString()},tasks,created_at:new Date().toISOString(),updated_at:new Date().toISOString()};
let events=[],offline=false;
const event=(type,payload={},summary='Fixture event',task_id=null)=>{const sequence=events.length+1;const e={event_id:'event-'+sequence,mission_id:mission.mission_id,sequence,event_type:type,payload,summary,task_id,occurred_at:new Date().toISOString()};events.push(e);return e};
const proposal={mission_id:mission.mission_id,request_id:'change-offline',instruction_request_id:'request-offline',expected_plan_revision:3,expected_plan_sha256:'plan3',summary:'新增离线运行支持；界面工作继续',rationale:'增加存储与离线验收，只确认新增范围。',added_roles:[],added_tasks:[{...tasks[1].spec,id:'offline',title:'实现离线存储和恢复',depends_on:[]}],updates:[{task_id:'qa',depends_on:['ui','data','offline'],acceptance_criteria:['原界面与离线恢复均通过'],required_checks:['node --test']}],cancel_task_ids:[],required_acceptance:['断网后仍能读取已保存内容']};
const wait=()=>({contract_version:'mission.wait.v1',plan_sha256:mission.plan_sha256,cursor:events.length,reason:'not_waiting',actor:'none',can_increase_budget:false,can_resume:false,environment_blocked:false});
const snapshot=()=>({...mission,effective_wait:wait()});
const activities=()=>mission.tasks.filter(t=>t.status==='running'||(t.status==='awaiting_verification'&&t.verification_run_id)).map(t=>({id:t.status==='running'?t.attempt_run_id:t.verification_run_id,workflow_run_id:t.status==='running'?t.attempt_run_id:t.verification_run_id,mission_id:mission.mission_id,mission_task_id:t.spec.id,source:'workflow',title:t.spec.title,status:'running',created_at:mission.created_at,updated_at:new Date().toISOString(),workspace_root:mission.workspace.root}));
agentApi.missionSnapshot=async()=>{if(offline)throw new Error('Fixture connection disconnected');return {mission:snapshot(),events:[...events],cursor:events.length,requires_snapshot:false}};
agentApi.listMissions=async()=>[snapshot()];
agentApi.submitMissionMessage=async(id,input)=>{window.workbenchFixture.submissions.push(input);const question=input.message.includes('为什么');const status=question?'answered':'awaiting_confirmation';event('mission_message_submitted',{request_id:input.request_id,input,status,answer:question?'验收依赖界面和数据实现，尚未交付。本次询问未修改计划。':'已分析影响，等待确认。'});return {request_id:input.request_id,status,answer:question?'已回答':'待确认'};};
agentApi.applyMissionPlanChange=async()=>{mission={...mission,plan_revision:4,plan_sha256:'plan4',confirmation:{...mission.confirmation,plan_sha256:'plan4'}};event('mission_instruction_status_changed',{request_id:'request-offline',status:'applied',answer:'宿主已应用增量变更，界面工作继续。'});return {status:'applied',mission:snapshot()};};
agentApi.withdrawMissionInstruction=async()=>({status:'not_applied'});
agentApi.getMediaProviderCatalog=async()=>({providers:[]});
agentApi.saveMissionWorkflow=async(id,workflowId,version,name)=>{window.workbenchFixture.savedWorkflow={missionId:id,workflowId,version,name};return {package:{id:workflowId,version,name},package_digest:'saved-fixture-digest'};};
const forbidden=async()=>{throw new Error('Only explicit in-memory fixture mutations permitted')};
for(const method of ['dispatchNextMissionTask','generateMissionPlan','createMissionHandoff','assignMissionRoles','controlMission'])agentApi[method]=forbidden;
function App(){
 const [version,setVersion]=useState(0);
 const [page,setPage]=useState('tasks');
 const catalog={missions:[snapshot()],activities:activities(),loading:false,error:'',refresh,upsert:noop};
 window.workbenchFixture={...(window.workbenchFixture||{submissions:[]}),setScene(scene){
  offline=scene==='disconnected';
  mission.status=scene==='completed'?'completed':'active';
  mission.tasks=tasks.map(t=>({...t,spec:{...t.spec}}));
  if(scene==='waiting')mission.tasks[1].status='ready';
  if(scene==='failed')mission.tasks[1].status='failed';
  if(scene==='verification'){mission.tasks[0].status='completed';mission.tasks[1].status='completed';mission.tasks[2]={...mission.tasks[2],status:'awaiting_verification',verification_run_id:'verify-qa'};}
  if(scene==='completed')mission.tasks=mission.tasks.map(t=>({...t,status:'completed',verification_passed:true}));
  if(scene==='confirmation'){event('mission_message_submitted',{request_id:'request-offline',input:{kind:'instruction',message:'增加离线能力，界面工作继续'},status:'awaiting_confirmation'});event('mission_plan_change_proposed',{proposal,status:'awaiting_authorization',write_scope_delta:['offline/'],read_scope_delta:[]});}
  if(scene==='many')mission.tasks=[...mission.tasks,...Array.from({length:65},(_,i)=>({...makeTask('queued-'+i,'待派发工作 '+i,['ui']),status:'blocked'}))];
  setVersion(v=>v+1);
 },addMessages(count){for(let i=0;i<count;i++)event('mission_message_submitted',{request_id:'message-'+i,input:{kind:'question',message:'历史进展询问 '+i},status:'answered',answer:'当前仍由两个独立分支并行推进。'});},get revision(){return mission.plan_revision}};
 return <Shell currentPage={page} approvalCount={0} workflowApprovalCount={0} identity={null} dashboardEnabled={false} agentVersion="fixture" updateBusy={false} currentTask={null} activeRunCount={0} activeRun={null} quickPluginViews={[]} onNavigate={target=>setPage(typeof target==='string'?target:target.page)} onOpenPluginView={noop} onOpenSettings={noop} onOpenDashboard={noop} onOpenBuiltinAi={noop} onCheckUpdate={noop} onOpenAgentDirectory={noop} onQuit={noop}>
 <TaskCenterPage currentTask={null} dashboardEnabled={false} missionCatalog={catalog} initialMissionId={mission.mission_id} onLoadTaskHistory={async()=>[]} onLoadLocalActivity={async()=>activities()} onOpenDashboard={noop}/>
 </Shell>;
}
createRoot(document.getElementById('root')).render(<App/>);
