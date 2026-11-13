import { createRoot } from 'react-dom/client';
import { useRef, useState } from 'react';
import { WorkspaceRegistryPage } from '../../src/pages/WorkspaceRegistryPage';
import { ConfirmProvider, useConfirm } from '../../src/components/ConfirmDialog';
import { useModalFocus } from '../../src/components/useModalFocus';
import { agentApi } from '../../src/services/agentApi';
import '../../styles.css';

const params = new URLSearchParams(location.search);
const scenario = params.get('scenario') || 'normal';
const stamp = '2026-11-11T08:00:00Z';
const project = (id, name, parent_id = undefined) => ({ id, name, parent_id, kind: 'product', aliases: ['synthetic-alias'], context: { notes: '合成项目' } });
const projects = scenario === 'empty' ? [] : [
  project('p1', '审查项目'),
  project('p2', '用于验证四十个汉字上限及窄窗口显示的合成超长项目名称不能挤掉操作和目录数量', 'p1'),
  project('p3', '没有关联目录的项目'),
  ...(scenario === 'many-projects' ? Array.from({length:60},(_,i)=>project(`extra-${i}`,`合成项目 ${i+1}`)) : []),
];
const workspaces = projects.length ? [
  { id:'w1',project_id:'p1',path:'F:/synthetic/workspace/'+'very-long-directory-name-'.repeat(8),role:'root',source:'manual',confidence:'exact',created_at:stamp },
  { id:'w2',project_id:'p1',path:'F:/synthetic/worktree',role:'worktree',source:'manual',confidence:'inferred',created_at:stamp },
  ...(scenario === 'many-projects' ? Array.from({length:40},(_,i)=>({id:`extra-w${i}`,project_id:'p1',path:`F:/synthetic/directory-${i}`,role:'root',source:'manual',confidence:'exact',created_at:stamp})) : []),
] : [];
const node = p => ({project:p,paths:workspaces.filter(w=>w.project_id===p.id),children:projects.filter(c=>c.parent_id===p.id).map(node)});
const snapshot = {projects,workspaces,tree:projects.filter(p=>!p.parent_id).map(node),updated_at:stamp};
let reads = 0;
window.workspaceFixture = { reads:0, saves:0, saveIds:[], picks:0, binds:0, historyReads:0, transcriptReads:0 };
agentApi.workspaceRegistry = async () => {
  reads += 1; window.workspaceFixture.reads = reads;
  window.dispatchEvent(new CustomEvent('workspace-review-read',{detail:reads}));
  if (scenario === 'registry-error' && reads === 1) throw new Error('SYNTHETIC_REGISTRY_READ_FAILURE');
  if (scenario === 'slow-refresh' && reads > 1) await new Promise(resolve=>setTimeout(resolve,2200));
  return snapshot;
};
agentApi.listWorkspaceCandidates = async () => ({candidates:scenario === 'empty' ? [] : [{path:'F:/synthetic/unassigned/'+'long-name-'.repeat(10),suggested_project:'待确认合成项目',confidence:'inferred',source:'synthetic',seen_at:stamp,seen_count:3,status:'pending'}]});
const rustFixture = await fetch('/__workspace-source-wire').then(r => r.json());
const wire = rustFixture.source_keys;
const sources = scenario === 'unknown-source' ? [...wire.slice(0,4), 'future_client'] : scenario === 'legacy-source' ? [...wire.slice(0,4),'open_code'] : wire;
agentApi.listWorkspaceSessionHistory = async (projectId,cursor,query,source) => {
  window.workspaceFixture.historyReads += 1;
  if (scenario === 'slow-history' && window.workspaceFixture.historyReads > 1) await new Promise(resolve=>setTimeout(resolve,900));
  if (scenario === 'history-error' && window.workspaceFixture.historyReads === 1) throw new Error('SESSION_CURSOR_INVALID');
  const all = Array.from({length:scenario === 'one-session' ? 1 : 55},(_,i)=>({...rustFixture.history.sessions[i%5],id:sources[i%5]+':s'+i,source:sources[i%5],native_id:'s'+i,project_id:projectId,title:'Synthetic session '+String(i+1).padStart(3,'0'),working_directory:'F:/synthetic/'+'workspace-'.repeat(25)}));
  const rows = projectId === 'p3' ? [] : all.filter(s=>(!source || s.source===source) && (!query || s.title.includes(query)));
  const offset = Number(cursor || 0);
  return {sessions:rows.slice(offset,offset+50),total:rows.length,next_cursor:offset+50<rows.length?String(offset+50):null,sources:rustFixture.history.sources.map((status,i)=>({...status,source:sources[i]}))};
};
agentApi.getWorkspaceSessionTranscriptPage = async (_projectId,sessionId,cursor) => {
window.workspaceFixture.transcriptReads += 1;
if(scenario === 'transcript-error' && window.workspaceFixture.transcriptReads === 1) throw new Error('SYNTHETIC_TRANSCRIPT_FAILURE');
if(scenario === 'slow-transcript') await new Promise(resolve=>setTimeout(resolve,1000));
return ({session_id:sessionId,through_seq:null,messages:Array.from({length:5},(_,i)=>({id:String(i),role:i%2?'assistant':'user',content:'合成正文 '+('verylongword'.repeat(35)),timestamp:stamp})),next_cursor:cursor?null:'50'});
};
// In-memory simulation only: never calls the running agent or local persistence.
agentApi.upsertWorkspaceProject = async input => {
  window.workspaceFixture.saves += 1;
  window.workspaceFixture.saveIds.push(input.id ?? null);
  if(scenario === 'bind-conflict' && input.initial_path === 'F:/synthetic/chosen') throw new Error('PATH_CONFLICT: synthetic directory');
  if(scenario === 'save-error' && window.workspaceFixture.saves === 1) throw new Error('SYNTHETIC_SAVE_FAILURE');
  if(scenario === 'slow-save') await new Promise(resolve=>setTimeout(resolve,1200));
  return {...input,id:input.id || 'synthetic-new'};
};
agentApi.deleteWorkspaceProject = async () => { throw new Error('PROJECT_HAS_CHILDREN'); };
agentApi.pickWorkspaceDirectory = async () => {
  window.workspaceFixture.picks += 1;
  if(scenario === 'pick-error' && window.workspaceFixture.picks === 1) throw new Error('SYNTHETIC_PICK_FAILURE');
  return {path:scenario === 'bind-conflict' && window.workspaceFixture.picks > 1 ? 'F:/synthetic/corrected' : 'F:/synthetic/chosen'};
};
agentApi.bindWorkspacePath = async input => {
  window.workspaceFixture.binds += 1;
  if(scenario === 'bind-conflict' && input.path === 'F:/synthetic/chosen') throw new Error('PATH_CONFLICT: synthetic directory');
};
agentApi.acceptWorkspaceCandidate = async () => { window.workspaceFixture.binds += 1; };
agentApi.dismissWorkspaceCandidate = async () => {};
agentApi.validateWorkspaceRegistry = async () => ({ok:true,problems:[],missing_paths:[],project_count:projects.length,workspace_count:workspaces.length});
window.workspaceFixture.openedPaths = [];
agentApi.openFolder = async path => { window.workspaceFixture.openedPaths.push(path); };
const content = <WorkspaceRegistryPage />;
const shell = params.get('wrapper') === 'shell';
function LegacyModalFixture() {
  const [open, setOpen] = useState(true);
  const ref = useRef(null);
  const confirm = useConfirm();
  useModalFocus(ref, open, () => setOpen(false));
  return open ? <div className="modal-backdrop"><div className="modal legacy-modal-fixture" role="dialog" ref={ref} aria-label="旧版模态回归"><button className="btn" onClick={() => void confirm({title:'合成确认',confirmText:'确认'})}>打开合成确认</button></div></div> : null;
}
createRoot(document.getElementById('root')).render(<ConfirmProvider>{shell ? <div className="shell"><div className="app-menu-bar" aria-hidden="true">审查用桌面外壳</div><div className="shell-body"><aside className="sidebar" aria-hidden="true" /><main className="main"><div className="main-content">{content}</div></main></div></div> : <main style={{padding:'16px',height:'100dvh',display:'flex',minHeight:0}}>{content}</main>}{scenario === 'legacy-modal' ? <LegacyModalFixture /> : null}</ConfirmProvider>);
