import React, {useCallback, useState} from 'react';
import {createRoot} from 'react-dom/client';
import {CollaborationTaskDetail} from '../../src/pages/MissionTeamPanel';
import {TaskCenterPage} from '../../src/pages/TaskCenterPage';
import {InboxPage} from '../../src/pages/InboxPage';
import {useMissionCatalog} from '../../src/pages/useMissionCatalog';
import {missionTodo} from '../../src/pages/taskCenterModel';
import {agentApi} from '../../src/services/agentApi';
import '../../styles.css';

// Only the native transport is replaced. React, hooks, timers, detail polling,
// snapshot pagination, budget dialog and evidence components are production code.
(window as any).__TAURI_INTERNALS__ = {invoke: async (command: string, args = {}) => {
  const response = await fetch('/clock-evidence-transport', {
    method:'POST', headers:{'Content-Type':'application/json'},
    body:JSON.stringify({command, ...args, browserNow:Date.now()}),
  });
  const result = await response.json();
  if (!result.ok) throw new Error(result.error);
  return result.value;
}};
const noop = () => {};
const refresh = async () => {};
const empty = async () => [];
function RecoverySurfaces() {
  const catalog = useMissionCatalog();
  const [surface, setSurface] = useState('tasks');
  const [missionId, setMissionId] = useState('');
  const select = useCallback((id: string) => setMissionId(id), []);
  const todos = catalog.missions.flatMap(mission => { const todo = missionTodo(mission, catalog.activities); return todo ? [todo] : []; });
  return <><nav aria-label="回归入口"><button className="btn" onClick={() => {setSurface('tasks'); setMissionId('');}}>任务入口</button><button className="btn" onClick={() => setSurface('inbox')}>待处理入口</button></nav>
    {surface === 'tasks' ? <TaskCenterPage currentTask={null} dashboardEnabled={false} missionCatalog={catalog} initialMissionId={missionId} onMissionSelectionChange={select} onLoadTaskHistory={empty} onLoadLocalActivity={agentApi.localActivity} onOpenDashboard={noop}/>
      : <InboxPage approvals={[]} workflowRuns={[]} missionTodos={todos} missionError={catalog.error} onOpenMission={id => {setMissionId(id); setSurface('tasks');}} onRefresh={catalog.refresh} onRespond={noop} onOpenWorkflowRun={noop} onOpenApprovalHistory={noop}/>}</>;
}
createRoot(document.getElementById('root')!).render(
  <main className="main"><div className="main-content">
    {new URLSearchParams(location.search).has('surfaces') ? <RecoverySurfaces/>
      : <CollaborationTaskDetail missionId={new URLSearchParams(location.search).get('id') || 'clock-fixture'} onBack={noop} onRefresh={refresh}/>}
  </div></main>,
);
