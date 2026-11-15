import React, {useRef,useState} from 'react';
import {createRoot} from 'react-dom/client';
import {TaskCenterPage} from '/src/pages/TaskCenterPage';
import {InboxPage} from '/src/pages/InboxPage';
import {WorkflowsPage} from '/src/pages/WorkflowsPage';
import {useMissionCatalog} from '/src/pages/useMissionCatalog';
import {EMPTY_TASK_CONTEXT,missionTodo} from '/src/pages/taskCenterModel';
import {agentApi} from '/src/services/agentApi';
import '/styles.css';
const stamp='2026-11-11T08:00:00Z';
const spec={id:'t1',title:'修复搜索',objective:'支持中文',depends_on:[],read_paths:['src'],write_paths:['src'],acceptance_criteria:['搜索通过'],required_checks:[],execution:{role_id:'reviewer',executor:'himind.builtin',model:''},repair_of:''};
const base={schema_version:'mission.v1',objective:'修复任务搜索并提供验收证据',workspace:{root:'F:/project',read_scope:['.'],write_scope:['src']},acceptance_criteria:['中文搜索可用'],plan_revision:1,plan_sha256:'hash1',plan_summary:'修复搜索并验收',required_acceptance:[],proposed_tasks:[spec],budget:null,usage:null,handoff:{source_client:'codex',context_summary:'从当前会话交接'},workflow:null,assignment:null,confirmation:null,tasks:[],created_at:stamp,updated_at:stamp};
let missions=[
 {...base,mission_id:'m-confirm',title:'搜索改造',status:'awaiting_confirmation'},
 {...base,mission_id:'m-assign',title:'待指派任务',status:'awaiting_assignment'},
 {...base,mission_id:'m-done',title:'已交付任务',status:'completed',tasks:[{spec,status:'completed',attempt_run_id:'mission-run',verification_passed:true,evidence_refs:['F:/project/results/搜索报告.md']}]},
 {...base,mission_id:'m-verification',title:'待验收任务',status:'active',confirmation:{plan_sha256:'hash1',selected_task_ids:['t1'],approved_absolute_read_paths:[],approved_absolute_write_paths:[],approved_paths:['src'],confirmed_by:'fixture',confirmed_at:stamp},tasks:[{spec,status:'awaiting_verification',attempt_run_id:'mission-implementation',verification_run_id:'',verification_passed:null,evidence_refs:['local-run:mission-implementation']}]},
 {...base,mission_id:'m-wait',title:'暂停的协作任务',status:'waiting'},
 {...base,mission_id:'m-stalled',title:'验收未通过的协作任务',status:'active',budget:{max_repair_attempts:0},tasks:[{spec:{...spec,id:'failed-fix'},status:'failed',attempt_run_id:'failed-run',verification_run_id:'failed-verification',verification_passed:false,evidence_refs:['local-run:failed-run']},{spec:{...spec,id:'blocked-review',depends_on:['failed-fix']},status:'blocked',attempt_run_id:'',verification_run_id:'',verification_passed:null,evidence_refs:[]}]},
];
const activity=(id,extra={})=>({id,workflow_run_id:id,source:'workflow',title:id,status:'completed',progress:100,step_done:1,step_total:1,created_at:stamp,updated_at:stamp,workspace_root:'F:/project',...extra});
const activities=[
 activity('mission-run',{mission_id:'m-done',mission_task_id:'t1'}),
 activity('child-run',{parent_run_id:'mission-run',parent_kind:'retry'}),
 activity('report-root',{title:'报告主任务',status:'running'}),
 activity('report-child',{title:'报告子执行',parent_run_id:'report-root'}),
 activity('external',{title:'外部等待',status:'waiting',pause_kind:'external'}),
 activity('feedback',{mission_id:'m-wait',mission_task_id:'t1',title:'反馈请求',status:'waiting',pause_kind:'feedback',current_step_id:'s1'}),
 activity('canceled',{title:'取消的任务',status:'canceled'}),
 ...Array.from({length:28},(_,i)=>activity('filler-'+i,{title:'报告记录 '+i})),
 activity('unknown',{title:'归属未知',workspace_root:'',mission_id:'missing'}),
];
window.fixtureMutations=[];
agentApi.listMissions=async()=>missions.map(m=>({...m}));
agentApi.missionSnapshot=async(id)=>({mission:{...missions.find(m=>m.mission_id===id)},events:[],cursor:0});
agentApi.confirmMissionPlan=async(id)=>{
 window.fixtureMutations.push('confirm-memory-only');
 missions=missions.map(m=>m.mission_id===id?{...m,status:'active',tasks:[{spec,status:'running',attempt_run_id:'new-run',verification_passed:null,evidence_refs:[]}]}:m);
 return missions.find(m=>m.mission_id===id);
};
const forbidden=async()=>{throw new Error('Mutation forbidden in read-only task fixture');};
for(const name of ['dispatchNextMissionTask','controlMission','generateMissionPlan','assignMissionRoles','createMissionHandoff'])agentApi[name]=forbidden;
const loadActivity=async()=>activities;
const loadHistory=async()=>[];
const noop=()=>{};
const workflowProps={
 snapshot:{workflows:[],runs:[]},mode:'runs',loading:false,error:'',onRefresh:noop,
 onLoadRun:async(id)=>({run:{run_id:id,interaction_id:'i1',status:'succeeded',current_step_id:'',runtime_provider:'himind.builtin',workspace_ref:'',steps:[],approvals:[],artifacts:[],error:'',created_at:'1794370000',updated_at:'1794370010'},interaction:null,events:[],projections:[],workflow:null}),
 onVerify:forbidden,onRevealArtifact:forbidden,onApprove:forbidden,onReject:forbidden,onResume:forbidden,onCancel:forbidden,
 onStart:forbidden,onPreflight:forbidden,onSaveCredentialFile:forbidden,onSaveCredentialSecret:forbidden,onInstallLocal:forbidden,
 onSetEnabled:forbidden,onRemove:forbidden,onRemoveDevelopment:forbidden,onPickDirectory:forbidden,onOpenExtensions:noop,onScheduleWorkflow:noop,onLoadPresets:async()=>({presets:[]}),onSavePreset:forbidden,onDeletePreset:forbidden,
};
function App(){
 const catalog=useMissionCatalog();
 const [page,setPage]=useState('tasks');const [missionId,setMissionId]=useState('');const [runId,setRunId]=useState('');
 const saved=useRef(EMPTY_TASK_CONTEXT);const save=useRef(c=>{saved.current=c}).current;
 const openMission=useRef(id=>setMissionId(id)).current;
 const todos=catalog.missions.flatMap(m=>{const todo=missionTodo(m);return todo?[todo]:[]});
 return <div style={{height:'100vh',display:'flex',flexDirection:'column'}}>
  <nav><button onClick={()=>{setMissionId('');setPage('tasks')}}>任务入口</button><button onClick={()=>setPage('inbox')}>待处理入口</button><span data-testid="badge">{todos.length+1}</span></nav>
  <main className="main"><div className="main-content">
   {page==='tasks'?<TaskCenterPage currentTask={null} dashboardEnabled={false} missionCatalog={catalog} initialMissionId={missionId} initialContext={saved.current} onContextChange={save} onMissionSelectionChange={openMission} onLoadTaskHistory={loadHistory} onLoadLocalActivity={loadActivity} onOpenDashboard={noop} onOpenWorkflowRun={id=>{setRunId(id);setPage('run')}} onApproveRun={forbidden} onRejectRun={forbidden}/>
    :page==='inbox'?<InboxPage approvals={[]} workflowRuns={[{workflow_name:'反馈请求',waiting_kind:'feedback',run:{run_id:'feedback',current_step_id:'s1',status:'waiting',updated_at:stamp}}]} missionTodos={todos} onOpenMission={id=>{setMissionId(id);setPage('tasks')}} onRefresh={catalog.refresh} onRespond={noop} onOpenWorkflowRun={noop} onOpenApprovalHistory={noop}/>
    :<WorkflowsPage {...workflowProps} initialRunId={runId} onBackToTasks={()=>setPage('tasks')}/>}
  </div></main>
 </div>;
}
createRoot(document.getElementById('root')).render(<App/>);
