// Synthetic in-memory fixture: no Tauri bridge, filesystem writes, or private sessions.
import { createRoot } from 'react-dom/client';
import { WorkspaceRegistryPage } from '../../src/pages/WorkspaceRegistryPage';
import { ConfirmProvider } from '../../src/components/ConfirmDialog';
import { agentApi } from '../../src/services/agentApi';
import { descendantProjectIds, projectDisplayName, projectSearchText } from '../../src/pages/workspaceProjectModel';
import '../../styles.css';

const make = (id, name, aliases = [], parent_id = '') => ({ id, name, aliases, parent_id, kind: 'product' });
let projects = [make('root', 'IODevice', ['IO 设备平台']), make('sdk', '核心驱动与 SDK', ['IODevice SDK'], 'root'), make('studio', 'IOStudio', ['设备配置与编排'], 'root'), make('plugins', '设备插件', ['IOUI'], 'root'), make('cinema', 'CinemaControl', ['动感影院播控'], 'root'), make('player', '交互播放器', ['UNIPlayer'], 'root'), make('ambiguous', 'TechnicalTool', ['工具甲', '工具乙'])];
const technicalPaths = ['O:/synthetic/IODevice', 'O:/synthetic/IODevice/IODevice', 'O:/synthetic/IODevice/IODevice/IOStudio', 'O:/synthetic/IODevice/IOUI', 'O:/synthetic/IODevice/CinemaControl', 'F:/synthetic/UNIPlayer'];
let paths = projects.slice(0, 6).map((p, index) => ({ id: `w${index}`, project_id: p.id, path: technicalPaths[index], role: 'root', source: 'manual', confidence: 'exact' }));
paths.push({ id: 'to-convert', project_id: 'root', path: 'O:/synthetic/IODevice/NewTool', role: 'root', source: 'manual', confidence: 'exact' });
let revision = 'v1';
let backup;
const wire = ['dsh', 'acp', 'claude_code', 'codex', 'opencode'];
const fixture = window.hierarchyFixture = { calls: [], actions: [], saves: [], stale: false };
const record = (kind, data) => fixture.calls.push({ kind, ...data });
const snapshot = () => {
  const node = project => ({ project, paths: paths.filter(p => p.project_id === project.id), children: projects.filter(p => p.parent_id === project.id).map(node) });
  return { projects: [...projects], workspaces: [...paths], tree: projects.filter(p => !p.parent_id).map(node), updated_at: revision, revision };
};
agentApi.workspaceRegistry = async () => snapshot();
agentApi.listWorkspaceCandidates = async () => ({ candidates: [] });
const all = () => Array.from({ length: 61 }, (_, i) => {
  const owner = projects[i % 6];
  return { id: `${wire[i % 5]}:session-${i}`, native_id: `session-${i}`, source: wire[i % 5], title: `合成会话 ${String(i).padStart(2, '0')}`, project_id: owner.id, project_name: projectDisplayName(owner), working_directory: paths.find(p => p.project_id === owner.id)?.path || 'O:/synthetic', updated_at: '2026-11-13T00:00:00Z', capabilities: { can_preview: true, can_resume: ['claude_code', 'codex'].includes(wire[i % 5]) } };
});
agentApi.listWorkspaceSessionHistory = async (projectId, cursor, query, source, sort, includeChildren) => {
  record('list', { projectId, includeChildren, cursor });
  const ids = includeChildren ? descendantProjectIds(projects, projectId) : new Set([projectId]);
  let rows = all().filter(s => ids.has(s.project_id) && (!query || `${s.title} ${s.working_directory} ${projectSearchText(projects.find(p => p.id === s.project_id))}`.toLowerCase().includes(query.toLowerCase())));
  const sources = wire.map(source => ({ source, availability: 'available', session_count: rows.filter(s => s.source === source).length }));
  rows = rows.filter(s => !source || s.source === source);
  if (sort === 'oldest') rows.reverse();
  const offset = Number(cursor || 0);
  return { sessions: rows.slice(offset, offset + 50), sources, total: rows.length, next_cursor: offset + 50 < rows.length ? String(offset + 50) : null };
};
agentApi.getWorkspaceSessionTranscriptPage = async (owner, sessionId) => {
  record('transcript', { owner, sessionId });
  const session = all().find(s => s.id === sessionId);
  if (!session || owner !== session.project_id) throw Error('SESSION_NOT_FOUND');
  return { session_id: sessionId, messages: Array.from({ length: 12 }, (_, i) => ({ id: `m${i + 1}`, role: i % 2 ? 'assistant' : 'user', content: `合成提问 needle ${i + 1}\n${'用于验证正文滚动与长文本换行。'.repeat(16)}`, files: [] })), next_cursor: null };
};
agentApi.getWorkspaceSessionResumePlan = async (owner, sessionId) => {
  record('plan', { owner, sessionId });
  if (all().find(s => s.id === sessionId)?.project_id !== owner) throw Error('SESSION_NOT_FOUND');
  return { source: 'codex', session_id: sessionId, program: 'synthetic.exe', command: 'synthetic --resume', working_directory: paths.find(p => p.project_id === owner)?.path, available: true };
};
agentApi.resumeWorkspaceSession = async (owner, sessionId) => { fixture.actions.push({ owner, sessionId }); };
let job;
agentApi.startWorkspaceSessionInspection = async (projectId, sessionId, query, mode, source, includeChildren) => { record('search', { projectId, includeChildren, sessionId }); job = { projectId, sessionId, query, mode, source, includeChildren }; return 'synthetic-token'; };
agentApi.stepWorkspaceSessionInspection = async () => {
  const ids = job.includeChildren ? descendantProjectIds(projects, job.projectId) : new Set([job.projectId]);
  const rows = all().filter(s => ids.has(s.project_id) && (!job.sessionId || s.id === job.sessionId) && (!job.source || s.source === job.source));
  return { token: 'synthetic-token', done: true, scanned_pages: rows.length, scanned_sessions: rows.length, total_sessions: rows.length, limited: false, cancelled: false, issues: [], sources: [], hits: rows.slice(0, 4).map(session => ({ session, message_id: 'm1', cursor: null, previous: [], excerpt: 'needle', paths: [] })) };
};
agentApi.cancelWorkspaceSessionInspection = async () => {};
agentApi.upsertWorkspaceProject = async input => {
  fixture.saves.push(input);
  const previous = projects.find(p => p.id === input.id);
  const saved = { ...previous, ...input, id: input.id || 'new-project', aliases: [...new Set([...(input.aliases || []), ...(previous ? [previous.name] : [])])].filter(a => a !== input.name) };
  projects = [...projects.filter(p => p.id !== saved.id), saved]; revision += 's'; return saved;
};
agentApi.pickWorkspaceDirectory = async () => ({ path: 'O:/synthetic/new' });
agentApi.openFolder = async path => fixture.actions.push({ path });
agentApi.validateWorkspaceRegistry = async () => ({ ok: true, problems: [], missing_paths: [], project_count: projects.length, workspace_count: paths.length });
agentApi.planWorkspaceSubprojects = async input => {
  if (fixture.stale || input.expected_revision !== revision) throw Error('WORKSPACE_REGISTRY_CHANGED');
  record('organize-preview', { input });
  return { revision, parent: { ...projects.find(p => p.id === input.parent_id), name: input.parent_name }, changes: input.entries.map(entry => ({ workspace_id: entry.workspace_id, path: paths.find(p => p.id === entry.workspace_id).path, previous_project_id: input.parent_id, project: make(`child-${entry.workspace_id}`, entry.name, entry.aliases, input.parent_id) })) };
};
agentApi.applyWorkspaceSubprojects = async input => {
  const preview = await agentApi.planWorkspaceSubprojects(input);
  backup = { projects: structuredClone(projects), paths: structuredClone(paths), revision };
  projects = projects.map(p => p.id === input.parent_id ? { ...p, name: input.parent_name, aliases: [...new Set([...p.aliases, p.name])] } : p).concat(preview.changes.map(c => c.project));
  paths = paths.map(path => ({ ...path, project_id: preview.changes.find(c => c.workspace_id === path.id)?.project.id || path.project_id }));
  revision += 'a';
  return { ...preview, rollback_revision: backup.revision, applied_revision: revision };
};
agentApi.restoreWorkspaceSubprojects = async (_before, expected) => { if (revision !== expected) throw Error('WORKSPACE_REGISTRY_CHANGED'); ({ projects, paths } = backup); revision += 'r'; };

createRoot(document.getElementById('root')).render(<ConfirmProvider><div className="shell"><div className="app-menu-bar">合成工作区预览</div><div className="shell-body"><main className="main"><div className="main-content"><WorkspaceRegistryPage /></div></main></div></div></ConfirmProvider>);
