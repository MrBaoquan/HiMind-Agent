import React, {useEffect, useRef, useState} from 'react';
import {createRoot} from 'react-dom/client';
import {ConfirmProvider} from '/src/components/ConfirmDialog';
import {Shell} from '/src/components/Shell';
import {TaskCenterPage} from '/src/pages/TaskCenterPage';
import {InboxPage} from '/src/pages/InboxPage';
import {DashboardPage} from '/src/pages/DashboardPage';
import {AiProvidersPanel} from '/src/pages/AiProvidersPanel';
import {WorkflowsPage} from '/src/pages/WorkflowsPage';
import {EMPTY_TASK_CONTEXT, missionTodo} from '/src/pages/taskCenterModel';
import {useMissionCatalog} from '/src/pages/useMissionCatalog';
import {agentApi} from '/src/services/agentApi';
import '/styles.css';
const noop=()=>{};
const forbidden=async()=>{throw new Error('Read-only desktop UI fixture');};
const stamp='2026-11-17T02:00:00Z';
const base={schema_version:'mission.v1',objective:'核对桌面布局',workspace:{root:'F:/some/long/workspace/desktop-review',read_scope:['.'],write_scope:[]},acceptance_criteria:['布局正确'],plan_revision:1,plan_sha256:'hash1',plan_summary:'计划',required_acceptance:[],proposed_tasks:[],budget:null,usage:null,handoff:{source_client:'codex',context_summary:''},workflow:null,assignment:null,confirmation:null,tasks:[],created_at:stamp,updated_at:stamp};
const missions=Array.from({length:12},(_,i)=>({...base,mission_id:'desktop-'+i,title:'桌面任务 '+i,status:'awaiting_confirmation'}));
missions[0].title+=' · '+ '很长的桌面任务标题'.repeat(12);
agentApi.listMissions=async()=>missions;
agentApi.missionSnapshot=async(id)=>({mission:missions.find(m=>m.mission_id===id),events:[],cursor:0});
agentApi.getMediaProviderCatalog=async()=>({providers:[]});
const emptyActivity=async()=>{window.desktopFixtureLocalLoads=(window.desktopFixtureLocalLoads||0)+1;return []};
const workflowProps={snapshot:{workflows:[],runs:[],library_issues:[{package_id:'com.himind.workflow.daily-work-log',version:'0.7.8',message:'workflow lock requires unavailable Plugin com.himind.work-log: plugin returned empty response'},{package_id:'com.himind.workflow.changed',version:'1',message:'workflow lock Plugin com.himind.example content changed'},{package_id:'unknown-package',version:'1',message:'invalid manifest: internal diagnostic'}]},mode:'library',loading:false,error:'',onRefresh:noop,onLoadRun:forbidden,onVerify:forbidden,onRevealArtifact:forbidden,onApprove:forbidden,onReject:forbidden,onResume:forbidden,onCancel:forbidden,onStart:forbidden,onPreflight:forbidden,onSaveCredentialFile:forbidden,onSaveCredentialSecret:forbidden,onInstallLocal:forbidden,onSetEnabled:forbidden,onRemove:forbidden,onRemoveDevelopment:forbidden,onPickDirectory:forbidden,onOpenExtensions:noop,onScheduleWorkflow:noop,onLoadPresets:async()=>({presets:[]}),onSavePreset:forbidden,onDeletePreset:forbidden};
function PersistentConversationProbe(){
 useEffect(()=>{window.desktopFixtureMounts=(window.desktopFixtureMounts||0)+1},[]);
 return <div hidden data-persistent-conversation />;
}
function App(){
 const [page,setPage]=useState('tasks'),[missionId,setMissionId]=useState('');
 const [aiState,setAiState]=useState('loading'),[workerOnline,setWorkerOnline]=useState(false);
 const saved=useRef(EMPTY_TASK_CONTEXT),save=useRef(c=>{saved.current=c}).current;
 const catalog=useMissionCatalog();
 window.desktopFixture={setAiState,setPage,setWorkerOnline};
 const navigate=target=>{setMissionId('');setPage(typeof target==='string'?target:target.page)};
 const todos=catalog.missions.flatMap(m=>{const t=missionTodo(m);return t?[t]:[]});
 const status={version:'test',mode:'connected',dashboard_enabled:true,dashboard_worker_online:workerOnline,current_task:null,provider:{available:true,enabled_by_user:true},dashboard_worker_error:'',dashboard_worker_reason_code:''};
 return <Shell currentPage={page} approvalCount={0} workflowApprovalCount={todos.length} identity={null} dashboardEnabled agentVersion="test" updateBusy={false} currentTask={null} activeRunCount={0} activeRun={null} quickPluginViews={Array.from({length:12},(_,i)=>({plugin_id:'fixture.plugin.'+i,plugin_name:'插件 '+i,view_id:'main',title:'工具 '+i,short_title:'工具 '+i}))} onNavigate={navigate} onOpenPluginView={noop} onOpenSettings={()=>setPage('ai')} onOpenDashboard={noop} onOpenBuiltinAi={noop} onCheckUpdate={noop} onOpenAgentDirectory={noop} onQuit={noop}>
 <PersistentConversationProbe />
 {page==='tasks'?<TaskCenterPage currentTask={null} dashboardEnabled={false} missionCatalog={catalog} initialMissionId={missionId} initialContext={saved.current} onContextChange={save} onMissionSelectionChange={setMissionId} onLoadTaskHistory={emptyActivity} onLoadLocalActivity={emptyActivity} onOpenDashboard={noop} onOpenWorkflowRun={noop} onApproveRun={forbidden} onRejectRun={forbidden}/>
 :page==='inbox'?<InboxPage approvals={[]} workflowRuns={[]} missionTodos={todos} onOpenMission={id=>{setMissionId(id);setPage('tasks')}} onRefresh={noop} onRespond={forbidden} onOpenWorkflowRun={noop} onOpenApprovalHistory={noop}/>
 :page==='dashboard'?<DashboardPage status={status} providerStatus={status.provider} projectionSyncStatus={null} approvals={[]} remoteExecutionSettings={null} mcpTargets={[]} localUsage={null} localUsageRange="today" localUsageBusy={false} inferenceGateway={null} identity={{authorized:true,user_name:'桌面审查账号',state:'authorized'}} authorization={null} identityBusy={false} updateStatus={null} updateBusy={false} projectionRequeueBusy={false} onOpenDashboard={noop} onOpenWorkbenchSettings={noop} onRequeueProjectionDeadLetters={noop} onStartAuthorization={noop} onStartEnrollment={noop} onCancelAuthorization={noop} onOpenAuthorization={noop} onRefreshIdentity={noop} onRevokeAuthorization={noop} onLocalUsageRangeChange={noop} onRefreshLocalUsage={noop} onBindCodexToGateway={noop} bindingModeBusy={false} onCheckUpdate={noop} onDownloadUpdate={noop} onInstallUpdate={noop}/>
 :page==='workflows'?<WorkflowsPage {...workflowProps} onInspectPlugins={()=>setPage('installed')}/>
 :page==='ai'?<AiProvidersPanel aiServices={aiState==='empty'?{custom:{services:[]},managed:{available:false},clients:{targets:[]}}:null} loadError={aiState==='error'?'AI 服务读取失败，请重试':''} templates={null} onRefresh={()=>setAiState('loading')} onSaveAIService={forbidden} onRemoveAIService={forbidden} onCloneAIService={forbidden} onOpenAccount={noop} onFetchModels={forbidden} onFetchSavedModels={forbidden} onFetchBalance={forbidden}/>
 :<h1>已装插件</h1>}
 </Shell>;
}
createRoot(document.getElementById('root')).render(<ConfirmProvider><App/></ConfirmProvider>);
