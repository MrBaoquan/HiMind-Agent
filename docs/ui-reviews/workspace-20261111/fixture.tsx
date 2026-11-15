import { createRoot } from 'react-dom/client';
import { WorkspaceRegistryPage } from '/src/pages/WorkspaceRegistryPage';
import { ConfirmProvider } from '/src/components/ConfirmDialog';
import { agentApi } from '/src/services/agentApi';
import '/styles.css';

const params = new URLSearchParams(location.search);
const scenario = params.get('scenario') || 'normal';
const stamp = '2026-11-11T08:00:00Z';
const project = (id, name, parent_id = undefined) => ({ id, name, parent_id, kind: 'product', aliases: ['synthetic-alias'], context: { notes: '合成项目' } });
const projects = scenario === 'empty' ? [] : [
  project('p1', '审查项目'),
  project('p2', '用于验证四十个汉字上限及窄窗口显示的合成超长项目名称不能挤掉操作和目录数量', 'p1'),
  project('p3', '没有关联目录的项目'),
];
const workspaces = projects.length ? [
  { id:'w1',project_id:'p1',path:'F:/synthetic/workspace/'+'very-long-directory-name-'.repeat(8),role:'root',source:'manual',confidence:'exact',created_at:stamp },
  { id:'w2',project_id:'p1',path:'F:/synthetic/worktree',role:'worktree',source:'manual',confidence:'inferred',created_at:stamp },
] : [];
const node = p => ({project:p,paths:workspaces.filter(w=>w.project_id===p.id),children:projects.filter(c=>c.parent_id===p.id).map(node)});
const snapshot = {projects,workspaces,tree:projects.filter(p=>!p.parent_id).map(node),updated_at:stamp};
let reads = 0;
agentApi.workspaceRegistry = async () => {
  reads += 1;
  window.dispatchEvent(new CustomEvent('workspace-review-read',{detail:reads}));
  if (scenario === 'registry-error') throw new Error('SYNTHETIC_REGISTRY_READ_FAILURE');
  if (scenario === 'slow-refresh' && reads > 1) await new Promise(resolve=>setTimeout(resolve,2200));
  return snapshot;
};
agentApi.listWorkspaceCandidates = async () => ({candidates:scenario === 'empty' ? [] : [{path:'F:/synthetic/unassigned/'+'long-name-'.repeat(10),suggested_project:'待确认合成项目',confidence:'inferred',source:'synthetic',seen_at:stamp,seen_count:3,status:'pending'}]});
const sources = ['dsh','acp','claude_code','codex',scenario === 'wire-source' ? 'open_code' : 'opencode'];
agentApi.listWorkspaceSessionHistory = async (projectId,cursor,query,source) => {
  if (scenario === 'history-error') throw new Error('SESSION_CURSOR_INVALID');
  const all = Array.from({length:55},(_,i)=>({id:sources[i%5]+':s'+i,source:sources[i%5],native_id:'s'+i,project_id:projectId,title:'Synthetic session '+String(i+1).padStart(3,'0'),working_directory:'F:/synthetic/'+'workspace-'.repeat(25),updated_at:stamp,message_count:101,capabilities:{can_preview:true,can_open_original:false,can_resume:false}}));
  const rows = projectId === 'p3' ? [] : all.filter(s=>(!source || s.source===source) && (!query || s.title.includes(query)));
  const offset = Number(cursor || 0);
  return {sessions:rows.slice(offset,offset+50),total:rows.length,next_cursor:offset+50<rows.length?String(offset+50):null,sources:sources.map((source,i)=>({source,availability:['unavailable','available','available','partial','error'][i],session_count:11,error_code:i===0?'runtime_not_running':i===3?'scan_partial':i===4?'database_unreadable':null}))};
};
agentApi.getWorkspaceSessionTranscriptPage = async (_projectId,sessionId,cursor) => ({session_id:sessionId,through_seq:null,messages:Array.from({length:5},(_,i)=>({id:String(i),role:i%2?'assistant':'user',content:'合成正文 '+('verylongword'.repeat(35)),timestamp:stamp})),next_cursor:cursor?null:'50'});
const forbidden = async () => { throw new Error('Audit fixture: persistent mutations are disabled'); };
for (const method of ['upsertWorkspaceProject','deleteWorkspaceProject','bindWorkspacePath','acceptWorkspaceCandidate','dismissWorkspaceCandidate','pickWorkspaceDirectory']) agentApi[method] = forbidden;
agentApi.validateWorkspaceRegistry = async () => ({ok:true,problems:[],missing_paths:[],project_count:projects.length,workspace_count:workspaces.length});
const content = <WorkspaceRegistryPage />;
const shell = params.get('wrapper') === 'shell';
createRoot(document.getElementById('root')).render(<ConfirmProvider>{shell ? <div className="shell"><div className="app-menu-bar" aria-hidden="true">审查用桌面外壳</div><div className="shell-body"><aside className="sidebar" aria-hidden="true" /><main className="main"><div className="main-content">{content}</div></main></div></div> : <main style={{padding:'16px'}}>{content}</main>}</ConfirmProvider>);
