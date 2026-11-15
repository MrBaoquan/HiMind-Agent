import React,{useState,useCallback} from 'react';
import {createRoot} from 'react-dom/client';
import {TaskCenterPage} from '/src/pages/TaskCenterPage';
import {InboxPage} from '/src/pages/InboxPage';
import {useMissionCatalog} from '/src/pages/useMissionCatalog';
import {missionTodo} from '/src/pages/taskCenterModel';
import '/styles.css';

// Exercise agentApi's actual Tauri command marshalling, bridged only at transport.
const mode=new URLSearchParams(location.search).get('mode') || 'store';
window.__TAURI_INTERNALS__={invoke:async(command,args={})=>{
 const response=await fetch('/ui-store-probe',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({command,...args,mode})});
 const result=await response.json(); if(!result.ok) throw new Error(result.error); return result.value;
}};
const loadEmpty=async()=>[]; const noop=()=>{};
function App(){
 const catalog=useMissionCatalog();const [page,setPage]=useState('tasks');const [missionId,setMissionId]=useState('');
 const select=useCallback(id=>setMissionId(id),[]);
 const todos=catalog.missions.flatMap(m=>{const todo=missionTodo(m);return todo?[todo]:[]});
 return <div style={{height:'100vh',display:'flex',flexDirection:'column'}}>
  <nav aria-label="验证入口"><button className="btn" onClick={()=>{setPage('tasks');setMissionId('')}}>任务入口</button><button className="btn" onClick={()=>setPage('inbox')}>待处理入口</button></nav>
  <main className="main"><div className="main-content">
   {page==='tasks'?<TaskCenterPage currentTask={null} dashboardEnabled={false} missionCatalog={catalog} initialMissionId={missionId} onMissionSelectionChange={select} onLoadTaskHistory={loadEmpty} onLoadLocalActivity={loadEmpty} onOpenDashboard={noop}/>
    :<InboxPage approvals={[]} workflowRuns={[]} missionTodos={todos} missionError={catalog.error} onOpenMission={id=>{setMissionId(id);setPage('tasks')}} onRefresh={catalog.refresh} onRespond={noop} onOpenWorkflowRun={noop} onOpenApprovalHistory={noop}/>}
  </div></main>
 </div>;
}
createRoot(document.getElementById('root')).render(<App/>);
