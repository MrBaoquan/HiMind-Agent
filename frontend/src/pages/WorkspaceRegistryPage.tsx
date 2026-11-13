import { useEffect, useMemo, useRef, useState } from 'react';
import { CircleAlert, ChevronDown, ChevronRight, Copy, FolderOpen, FolderTree, Pencil, Plus, RefreshCw, Search, Trash2, X } from 'lucide-react';
import {
  agentApi,
  type WorkspaceRegistryBinding,
  type WorkspaceRegistryCandidate,
  type WorkspaceRegistryProject,
  type WorkspaceRegistrySnapshot,
  type WorkspaceRegistryTree,
  type WorkspaceRegistryValidation,
  type WorkspaceSubprojectPreview,
} from '../services/agentApi';
import { BusyIndicator } from '../components/BusyIndicator';
import { useConfirm } from '../components/ConfirmDialog';
import { Pill } from '../components/Common';
import './WorkspaceRegistryPage.css';
import { WorkspaceSessionHistoryPanel } from './WorkspaceSessionHistoryPanel';
import { ModalSurface } from '../components/ModalSurface';
import { WorkspaceSubprojectsDialog } from './WorkspaceSubprojectsDialog';
import { WorkspaceFeedback, workspaceIssue, type WorkspaceIssue } from './WorkspaceFeedback';
import { chineseProjectAliases, descendantProjectIds, hasChinese, projectAncestors, projectDisplayName, projectQualifiedName, projectSearchText, projectTechnicalName } from './workspaceProjectModel';

/**
 * 工作区登记。
 *
 * 这一屏回答一个问题：这个目录属于哪个项目、它的上级是谁。
 * 它不显示工时与进度（那是工作日志的事），也不改变任何会话权限——
 * 「登记了」不等于「能在这里写代码」。
 *
 * 数据落 `data/workspace-registry.json`，走备份包；同一份表也供
 * `workspace.registry.*` 能力读取，AI 在对话里解析工作区用的就是它。
 */

const KIND_OPTIONS: Array<{ value: string; label: string }> = [
  { value: 'product', label: '产品' },
  { value: 'subproject', label: '子项目' },
  { value: 'aggregate', label: '聚合仓库' },
  { value: 'delivery', label: '交付' },
  { value: 'library', label: '库' },
  { value: 'docs', label: '文档' },
  { value: 'sandbox', label: '沙箱' },
  { value: 'other', label: '其他' },
];

const KIND_LABELS: Record<string, string> = Object.fromEntries(KIND_OPTIONS.map(item => [item.value, item.label]));

const ROLE_LABELS: Record<string, string> = {
  root: '项目目录',
  worktree: '工作树',
  mirror: '镜像',
  release: '发布目录',
  source: '源码',
  build: '构建产物',
};

type Tab = 'projects' | 'paths' | 'pending';

type EditState = {
  /** 空串表示新建。 */
  id: string;
  name: string;
  aliases: string;
  kind: string;
  parentId: string;
  stack: string;
  docs: string;
  rules: string;
  notes: string;
  initialPath: string;
};

function emptyEdit(): EditState {
  return { id: '', name: '', aliases: '', kind: 'product', parentId: '', stack: '', docs: '', rules: '', notes: '', initialPath: '' };
}

function splitList(value: string): string[] {
  return value.split(/[,，]/).map(part => part.trim()).filter(Boolean);
}

export function WorkspaceRegistryPage() {
  const confirm = useConfirm();
  const [tab, setTab] = useState<Tab>('projects');
  const [snapshot, setSnapshot] = useState<WorkspaceRegistrySnapshot | null>(null);
  const [candidates, setCandidates] = useState<WorkspaceRegistryCandidate[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState('');
  const [error, setError] = useState<WorkspaceIssue | null>(null);
  const [loadError, setLoadError] = useState<WorkspaceIssue | null>(null);
  const [notice, setNotice] = useState('');
  const [edit, setEdit] = useState<EditState | null>(null);
  const [organize, setOrganize] = useState<{ parent: WorkspaceRegistryProject; paths: WorkspaceRegistryBinding[]; revision: string; initialId?: string } | null>(null);
  const [rollback, setRollback] = useState<WorkspaceSubprojectPreview | null>(null);
  const [filter, setFilter] = useState('');
  const [pathFilter, setPathFilter] = useState('');
  const [selectedProjectId, setSelectedProjectId] = useState('');
  const [expandedProjectIds, setExpandedProjectIds] = useState<Set<string>>(() => new Set());
  const alive = useRef(true);
  const loadInFlight = useRef(false);
  const pageHeading = useRef<HTMLHeadingElement>(null);
  const [candidateProjects, setCandidateProjects] = useState<Record<string, string>>({});
  const [projectViews, setProjectViews] = useState<Record<string, 'children' | 'history' | 'directories'>>(() => {
    try {
      const stored = JSON.parse(localStorage.getItem('workspace-project-views.v1') || '{}');
      if (!stored || typeof stored !== 'object' || Array.isArray(stored)) return {};
      return Object.fromEntries(Object.entries(stored).filter((entry): entry is [string, 'children' | 'history' | 'directories'] => entry[1] === 'children' || entry[1] === 'history' || entry[1] === 'directories'));
    } catch { return {}; }
  });
  const [projectScopes, setProjectScopes] = useState<Record<string, boolean>>({});
  const historyVisited = useRef(new Set<string>());
  const initializedTree = useRef(false);
  const treeRef = useRef<HTMLUListElement>(null);

  async function load(showNotice = false) {
    if (loadInFlight.current) return;
    loadInFlight.current = true;
    setLoading(true);
    setLoadError(null);
    if (showNotice) setNotice('');
    try {
      const [next, pending] = await Promise.all([
        agentApi.workspaceRegistry(),
        agentApi.listWorkspaceCandidates(),
      ]);
      if (!alive.current) return;
      setSnapshot(next);
      setCandidates(pending.candidates);
      if (showNotice) setNotice('工作区已更新');
      setSelectedProjectId(current => current && next.projects.some(project => project.id === current)
        ? current
        : next.tree[0]?.project.id ?? next.projects[0]?.id ?? '');
      const knownIds = new Set(next.projects.map(project => project.id));
      setExpandedProjectIds(current => {
        const expanded = new Set([...current].filter(id => knownIds.has(id)));
        // 初次打开时展开根节点，先让项目层级可见；更深层由用户按需展开。
        if (!initializedTree.current) {
          for (const node of next.tree) if (node.children.length) expanded.add(node.project.id);
          initializedTree.current = true;
        }
        return expanded;
      });
    } catch (loadError) {
      if (alive.current) setLoadError(workspaceIssue(loadError, '工作区读取失败，请重新加载。'));
    } finally {
      loadInFlight.current = false;
      if (alive.current) setLoading(false);
    }
  }

  useEffect(() => {
    alive.current = true;
    void load();
    return () => { alive.current = false; };
  }, []);

  const projects = snapshot?.projects ?? [];
  const workspaces = snapshot?.workspaces ?? [];

  const projectName = (id: string) => {
    const project = projects.find(item => item.id === id);
    return project ? projectDisplayName(project) : id;
  };
  const selectedProject = projects.find(item => item.id === selectedProjectId) ?? null;
  const selectedPaths = selectedProject ? workspaces.filter(item => item.project_id === selectedProject.id) : [];
  const selectedChildren = projects.filter(project => project.parent_id === selectedProjectId)
    .sort((left, right) => projectDisplayName(left).localeCompare(projectDisplayName(right), 'zh-CN'));
  const ancestors = projectAncestors(projects, selectedProjectId);
  const includeChildren = selectedChildren.length > 0 && (projectScopes[selectedProjectId] ?? true);
  const scopeIds = descendantProjectIds(projects, selectedProjectId);
  const scopedPaths = includeChildren ? workspaces.filter(item => scopeIds.has(item.project_id)) : selectedPaths;
  const projectNames = Object.fromEntries(projects.map(project => [project.id, projectDisplayName(project)]));
  const savedView = projectViews[selectedProjectId];
  const projectView = savedView === 'children' && !selectedChildren.length ? 'history' : savedView ?? (selectedChildren.length ? 'children' : 'history');
  if (projectView === 'history') historyVisited.current.add(selectedProjectId);
  const setProjectView = (view: 'children' | 'history' | 'directories') => {
    setProjectViews(current => {
      const next = { ...current, [selectedProjectId]: view };
      try { localStorage.setItem('workspace-project-views.v1', JSON.stringify(next)); } catch { /* private storage */ }
      return next;
    });
  };
  function selectProject(id: string) {
    setSelectedProjectId(id);
    setExpandedProjectIds(current => new Set([...current, ...projectAncestors(projects, id).map(project => project.id)]));
  }
  useEffect(() => {
    const tree = treeRef.current;
    const item = tree?.querySelector<HTMLElement>('.workspace-registry-node.selected');
    if (!tree || !item) return;
    const top = item.offsetTop - tree.offsetTop;
    if (top < tree.scrollTop) tree.scrollTop = top;
    else if (top + item.offsetHeight > tree.scrollTop + tree.clientHeight) tree.scrollTop = top + item.offsetHeight - tree.clientHeight;
  }, [selectedProjectId, expandedProjectIds]);

  const visibleTree = useMemo(() => {
    const keyword = filter.trim().toLowerCase();
    if (!keyword) return snapshot?.tree ?? [];
    // 搜到子项目或目录时保留祖先链，避免失去它在项目树中的位置。
    const prune = (node: WorkspaceRegistryTree): WorkspaceRegistryTree | null => {
      const ownMatch = projectSearchText(node.project).includes(keyword)
        || node.paths.some(item => item.path.toLowerCase().includes(keyword));
      const children = node.children.map(prune).filter((item): item is WorkspaceRegistryTree => item !== null);
      if (!ownMatch && children.length === 0) return null;
      return { ...node, children: ownMatch ? node.children : children };
    };
    return (snapshot?.tree ?? []).map(prune).filter((item): item is WorkspaceRegistryTree => item !== null);
  }, [snapshot, filter]);

  const visiblePaths = useMemo(() => {
    const keyword = pathFilter.trim().toLowerCase();
    if (!keyword) return workspaces;
    return workspaces.filter(item => {
      const project = projects.find(project => project.id === item.project_id);
      return `${item.path} ${project ? projectSearchText(project) : ''}`.toLowerCase().includes(keyword);
    });
  }, [workspaces, projects, pathFilter]);

  function startCreate(parentId = '') {
    if (loading || busy) return;
    setEdit({ ...emptyEdit(), parentId });
    setNotice('');
    setError(null);
  }

  function startEdit(project: WorkspaceRegistryProject) {
    setEdit({
      id: project.id,
      name: projectDisplayName(project),
      aliases: (project.aliases ?? []).join(', '),
      kind: project.kind || 'product',
      parentId: project.parent_id ?? '',
      stack: (project.context?.stack ?? []).join(', '),
      docs: (project.context?.docs ?? []).map(item => item.path).join(', '),
      rules: (project.context?.rules ?? []).map(item => item.path).join(', '),
      notes: project.context?.notes ?? '',
      initialPath: '',
    });
    setNotice('');
    setError(null);
  }

  async function pickInitialDirectory() {
    setBusy('pick-directory');
    setError(null);
    try {
      const picked = await agentApi.pickWorkspaceDirectory();
      const path = (picked?.path ?? '').trim();
      if (path) setEdit(current => current ? { ...current, initialPath: path } : current);
    } catch (pickError) {
      setError(workspaceIssue(pickError, '目录未选择，请重试或取消。'));
    } finally {
      setBusy('');
    }
  }

  async function saveEdit() {
    if (!edit || loading || busy) return;
    if (!edit.name.trim()) {
      setError({ message: '请填写项目别名。' });
      return;
    }
    setBusy('save');
    setError(null);
    try {
      const saved = await agentApi.upsertWorkspaceProject({
        id: edit.id || undefined,
        name: edit.name.trim(),
        initial_path: edit.initialPath.trim() || undefined,
        create_only: !edit.id,
        aliases: splitList(edit.aliases),
        kind: edit.kind,
        // 空串表示「提升为根」，后端用三态字段区分「没传」与「显式清空」。
        parent_id: edit.parentId || null,
        stack: splitList(edit.stack),
        docs: splitList(edit.docs).map(path => ({ label: path, path })),
        rules: splitList(edit.rules).map(path => ({ label: path, path })),
        notes: edit.notes,
      });
      setSelectedProjectId(saved.id);
      setTab('projects');
      setNotice(edit.id
        ? `已保存：${edit.name}`
        : edit.initialPath.trim() ? `已创建项目并添加目录：${edit.name}` : `已新建项目：${edit.name}`);
      await load();
      setEdit(null);
    } catch (saveError) {
      setError(workspaceIssue(saveError, '项目未保存，请重试。'));
    } finally {
      setBusy('');
    }
  }

  function startOrganize(initialId?: string) {
    if (!selectedProject || !snapshot?.revision || busy || loading) return;
    setOrganize({ parent: selectedProject, paths: selectedPaths, revision: snapshot.revision, initialId });
  }
  async function restoreOrganization() {
    if (!rollback?.rollback_revision || !rollback.applied_revision || busy) return;
    if (!await confirm({ title: '恢复本次整理？', description: '将恢复整理前的项目和目录归属；存在后续修改时会停止恢复。', confirmText: '恢复整理' })) return;
    setBusy('restore-organization'); setError(null);
    try {
      await agentApi.restoreWorkspaceSubprojects(rollback.rollback_revision, rollback.applied_revision);
      setRollback(null); setNotice('已恢复本次整理'); await load();
    } catch (reason) { setError(workspaceIssue(reason, '未恢复整理结果，请重新核对。')); }
    finally { setBusy(''); }
  }

  async function removeProject(project: WorkspaceRegistryProject) {
    const ok = await confirm({
      title: `删除「${projectDisplayName(project)}」？`,
      description: '如果它还有子项目或绑定的路径，删除会被拒绝；先移走它们再删。',
      confirmText: '删除',
      danger: true,
    });
    if (!ok) return;
    setBusy(`delete:${project.id}`);
    setError(null);
    try {
      await agentApi.deleteWorkspaceProject(project.id);
      setNotice(`已删除：${projectDisplayName(project)}`);
      await load();
      if (edit?.id === project.id) setEdit(null);
    } catch (deleteError) {
      setError(workspaceIssue(deleteError, '项目未删除，请重试。'));
    } finally {
      setBusy('');
    }
  }

  async function bindPath(project: WorkspaceRegistryProject) {
    setBusy(`bind:${project.id}`);
    setError(null);
    try {
      const picked = await agentApi.pickWorkspaceDirectory();
      const path = (picked?.path ?? '').trim();
      if (!path) return;
      await agentApi.bindWorkspacePath({
        project_ref: project.id,
        path,
        role: 'root',
        source: 'manual',
        confidence: 'exact',
      });
      setNotice(`已添加目录到「${projectDisplayName(project)}」`);
      await load();
    } catch (bindError) {
      setError(workspaceIssue(bindError, '目录未关联，请重新选择目录。'));
    } finally {
      setBusy('');
    }
  }

  async function accessDirectory(path: string, action: 'open' | 'copy') {
    if (busy) return;
    setBusy(`${action}:${path}`);
    setError(null);
    setNotice(action === 'open' ? '正在打开目录…' : '正在复制路径…');
    try {
      if (action === 'open') await agentApi.openFolder(path);
      else await navigator.clipboard.writeText(path);
      setNotice(action === 'open' ? '已打开目录' : '已复制目录路径');
    } catch (reason) {
      setNotice('');
      setError(workspaceIssue(reason, action === 'open' ? '目录未打开，请检查目录是否存在后重试。' : '路径未复制，请选择路径文字后复制。'));
    } finally {
      setBusy('');
    }
  }

  async function unbind(item: WorkspaceRegistryBinding) {
    const ok = await confirm({
      title: '解除目录关联？',
      description: `解除关联后，此目录不再识别为项目；目录与项目资料保留。\n${item.path}`,
      confirmText: '解除关联',
      danger: true,
    });
    if (!ok) return;
    setBusy(`unbind:${item.path}`);
    setError(null);
    try {
      await agentApi.bindWorkspacePath({ path: item.path, unbind: true });
      setNotice('已解除目录关联');
      await load();
    } catch (unbindError) {
      setError(workspaceIssue(unbindError, '目录未解除关联，请重试。'));
    } finally {
      setBusy('');
    }
  }

  async function acceptCandidate(candidate: WorkspaceRegistryCandidate, mode: 'create' | 'bind', projectRef?: string) {
    setBusy(`candidate:${candidate.path}`);
    setError(null);
    try {
      await agentApi.acceptWorkspaceCandidate(candidate.path, mode, projectRef, candidate.suggested_parent);
      if (mode === 'bind' && projectRef) {
        setSelectedProjectId(projectRef);
        setTab('projects');
      }
      setNotice(mode === 'create' ? `已新建项目并添加目录：${candidate.path}` : `已添加目录：${candidate.path}`);
      await load();
    } catch (acceptError) {
      setError(workspaceIssue(acceptError, '目录未添加，请重试。'));
    } finally {
      setBusy('');
    }
  }

  async function dismissCandidate(candidate: WorkspaceRegistryCandidate) {
    setBusy(`candidate:${candidate.path}`);
    setError(null);
    try {
      await agentApi.dismissWorkspaceCandidate(candidate.path);
      setNotice('已忽略这条候选');
      await load();
    } catch (dismissError) {
      setError(workspaceIssue(dismissError, '候选未忽略，请重试。'));
    } finally {
      setBusy('');
    }
  }

  async function validate() {
    setBusy('validate');
    setError(null);
    try {
      const result: WorkspaceRegistryValidation = await agentApi.validateWorkspaceRegistry();
      if (result.ok) {
        setNotice(`检查通过：${result.project_count} 个项目，${result.workspace_count} 个目录`);
      } else {
        const issues = [...result.problems, ...result.missing_paths.map(path => `目录不存在：${path}`)];
        setError({ message: '部分目录需要处理，请核对诊断详情。', detail: issues.join('\n') });
      }
    } catch (validateError) {
      setError(workspaceIssue(validateError, '目录检查失败，请重试。'));
    } finally {
      setBusy('');
    }
  }

  const parentOptions = edit
    ? projects.filter(item => !descendantProjectIds(projects, edit.id).has(item.id)).sort((left, right) => projectDisplayName(left).localeCompare(projectDisplayName(right), 'zh-CN'))
    : [];

  const tabItems: Array<[Tab, string, number]> = [
    ['projects', '项目', projects.length],
    ['paths', '目录清单', workspaces.length],
    ['pending', '待确认目录', candidates.length],
  ];

  return (
    <section className="workspace-registry" aria-busy={loading}>
      <header className="workspace-registry-head">
        <div>
          <h2 ref={pageHeading} tabIndex={-1}>工作区</h2>
          <p>目录关联帮助 Agent 识别项目，不改变会话权限。</p>
        </div>
        <div className="actions-row">
          <button type="button" className="btn btn-icon" title="刷新工作区" aria-label="刷新工作区" onClick={() => void load(true)} disabled={loading || Boolean(busy)}>
            <RefreshCw size={15} />
          </button>
          <button type="button" className="btn" title="检查目录" onClick={() => void validate()} disabled={loading || Boolean(busy)}>
            <CircleAlert size={15} /><span className="workspace-registry-action-label">检查目录</span>
          </button>
          <button type="button" className="btn btn-primary" title="新建项目" onClick={() => startCreate()} disabled={loading || Boolean(busy)}>
            <Plus size={15} /><span className="workspace-registry-action-label">新建项目</span>
          </button>
        </div>
      </header>

      {loadError ? <WorkspaceFeedback issue={loadError} onRetry={() => void load(true)} retryLabel="重新加载" disabled={loading} /> : null}
      {error && !edit ? <WorkspaceFeedback issue={error} /> : null}
      <div className="workspace-registry-status" role="status">{loading && snapshot ? '正在刷新工作区…' : notice}</div>
      {rollback ? <div className="workspace-registry-restore"><span>已保存 {rollback.changes.length} 个子项目</span><button type="button" className="btn" disabled={Boolean(busy)} onClick={() => void restoreOrganization()}>恢复本次整理</button><button type="button" className="btn" onClick={() => setRollback(null)}>关闭提示</button></div> : null}

      <nav className="workspace-registry-tabs" role="tablist" aria-label="工作区视图">
        {tabItems.map(([key, label, count], index) => (
          <button
            key={key}
            type="button"
            role="tab"
            aria-selected={tab === key}
            aria-controls={`workspace-registry-panel-${key}`}
            tabIndex={tab === key ? 0 : -1}
            className={tab === key ? 'active' : ''}
            onClick={() => setTab(key)}
            onKeyDown={event => {
              if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
              event.preventDefault();
              const nextIndex = (index + (event.key === 'ArrowRight' ? 1 : tabItems.length - 1)) % tabItems.length;
              const nextTab = tabItems[nextIndex][0];
              setTab(nextTab);
              event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>('[role="tab"]')[nextIndex]?.focus();
            }}
          >
            {label}<span>{count}</span>
          </button>
        ))}
      </nav>

      {loading && !snapshot ? <BusyIndicator label="读取工作区" /> : null}

      {tab === 'projects' && snapshot ? (
        <div className="workspace-registry-view workspace-registry-project-layout" id="workspace-registry-panel-projects" role="tabpanel" aria-label="项目">
          {projects.length === 0 ? (
            <div className="workspace-registry-no-projects">
              <FolderTree size={22} aria-hidden="true" />
              <strong>还没有项目</strong>
              <span>新建项目后，可以为它添加一个或多个本机目录。</span>
              <button type="button" className="btn btn-primary" onClick={() => startCreate()} disabled={loading || Boolean(busy)}><Plus size={14} />新建项目</button>
            </div>
          ) : (
            <>
              <label className="workspace-registry-project-picker">
                <span>当前项目</span>
                <select value={selectedProjectId} onChange={event => selectProject(event.target.value)} aria-label="切换项目">
                  {projects.map(project => <option key={project.id} value={project.id}>{projectQualifiedName(projects, project)}</option>)}
                </select>
              </label>
              <aside className="workspace-registry-project-browser" aria-label="项目层级">
                <div className="workspace-registry-browser-head">
                  <label className="workspace-registry-search">
                    <Search size={14} aria-hidden="true" />
                    <span className="sr-only">搜索项目或目录</span>
                    <input
                      type="search"
                      value={filter}
                      onChange={event => setFilter(event.target.value)}
                      placeholder="搜索项目、别名或目录"
                      aria-label="搜索项目、别名或目录"
                    />
                  </label>
                  <span>主项目 {projects.filter(project => !project.parent_id).length} · 子项目 {projects.filter(project => project.parent_id).length}</span>
                </div>
                {visibleTree.length ? (
                  <ul ref={treeRef} className="workspace-registry-tree" tabIndex={0} aria-label="项目列表">
                    {visibleTree.map(node => renderTreeNode(node))}
                  </ul>
                ) : (
                  <p className="workspace-registry-tree-empty">没有匹配的项目或目录。</p>
                )}
              </aside>

              <section className="workspace-registry-project-detail" aria-labelledby="workspace-selected-project-title">
                {selectedProject ? (
                  <>
                    {ancestors.length ? <nav className="workspace-registry-breadcrumbs" aria-label="项目位置">
                      {ancestors.map(project => <span key={project.id}><button type="button" onClick={() => selectProject(project.id)}>{projectDisplayName(project)}</button><ChevronRight size={14} aria-hidden="true" /></span>)}
                      <span aria-current="page">{projectDisplayName(selectedProject)}</span>
                    </nav> : null}
                    <header className="workspace-registry-detail-head">
                      <div className="workspace-registry-detail-title">
                        <div className="workspace-registry-project-title-line">
                          <h3 id="workspace-selected-project-title">{projectDisplayName(selectedProject)}</h3>
                          <Pill kind="neutral">{KIND_LABELS[selectedProject.kind ?? 'other'] ?? selectedProject.kind}</Pill>
                          {projectTechnicalName(selectedProject) ? <span className="workspace-registry-technical-name">{projectTechnicalName(selectedProject)}</span> : null}
                        </div>
                      </div>
                      <div className="actions-row">
                        <button type="button" className="btn" title="新建子项目" onClick={() => startCreate(selectedProject.id)} disabled={loading || Boolean(busy)}><Plus size={14} /><span className="workspace-registry-action-label">新建子项目</span></button>
                        {selectedPaths.length ? <button type="button" className="btn" title={selectedPaths.length > 1 ? '选择要打开的项目目录' : selectedPaths[0].path} onClick={() => selectedPaths.length === 1 ? void accessDirectory(selectedPaths[0].path, 'open') : setProjectView('directories')} disabled={Boolean(busy)}><FolderOpen size={14} />打开目录</button> : null}
                        <button type="button" className="btn" title="编辑项目" onClick={() => startEdit(selectedProject)} disabled={loading || Boolean(busy)}>
                          <Pencil size={14} /><span className="workspace-registry-action-label">编辑项目</span>
                        </button>
                      </div>
                    </header>

                    <nav className="workspace-registry-detail-tabs" aria-label="项目内容">
                      {selectedChildren.length ? <button type="button" className={projectView === 'children' ? 'active' : ''} aria-pressed={projectView === 'children'} onClick={() => setProjectView('children')}>子项目 <span>{selectedChildren.length}</span></button> : null}
                      <button type="button" className={projectView === 'history' ? 'active' : ''} aria-pressed={projectView === 'history'} onClick={() => setProjectView('history')}>会话历史</button>
                      <button type="button" className={projectView === 'directories' ? 'active' : ''} aria-pressed={projectView === 'directories'} onClick={() => setProjectView('directories')}>项目目录 <span>{scopedPaths.length}</span></button>
                    {projectView !== 'children' && selectedChildren.length ? <label className="workspace-registry-scope">
                      <span>范围</span>
                      <select aria-label="项目数据范围" value={includeChildren ? 'descendants' : 'self'} onChange={event => setProjectScopes(current => ({ ...current, [selectedProjectId]: event.target.value === 'descendants' }))}>
                        <option value="self">仅此项目</option><option value="descendants">包含子项目</option>
                      </select>
                    </label> : null}
                    </nav>
                    {projectView === 'children' ? <section className="workspace-registry-detail-section" aria-label="子项目列表" tabIndex={0}>
                      <ul className="workspace-registry-child-list">
                        {selectedChildren.map(project => {
                          const paths = workspaces.filter(path => path.project_id === project.id);
                          const children = projects.filter(child => child.parent_id === project.id);
                          return <li key={project.id}>
                            <button type="button" className="workspace-registry-child-name" onClick={() => selectProject(project.id)}>
                              <strong>{projectDisplayName(project)}</strong>
                              <span>{projectTechnicalName(project) || project.description || KIND_LABELS[project.kind ?? 'other']}</span>
                            </button>
                            <span className="workspace-registry-child-meta">{children.length ? `${children.length} 个子项目 · ` : ''}{paths.length} 个直属目录</span>
                            <code className="workspace-registry-path" title={paths[0]?.path}>{paths[0]?.path || '暂无直属目录'}</code>
                            <div className="actions-row">
                              <button type="button" className="btn" onClick={() => selectProject(project.id)}>进入</button>
                              {paths.length ? <button type="button" className="btn" disabled={Boolean(busy)} onClick={() => {
                                if (paths.length === 1) void accessDirectory(paths[0].path, 'open');
                                else { selectProject(project.id); setProjectViews(current => ({ ...current, [project.id]: 'directories' })); }
                              }}>打开目录</button> : null}
                            </div>
                          </li>;
                        })}
                      </ul>
                    </section> : null}
                    <div className="workspace-registry-history-pane" hidden={projectView !== 'history'}>
                      {historyVisited.current.has(selectedProject.id) ? <WorkspaceSessionHistoryPanel key={`${selectedProject.id}:${includeChildren}:${snapshot.revision ?? snapshot.updated_at}`} projectId={selectedProject.id} includeChildren={includeChildren} projectNames={projectNames} /> : null}
                    </div>
                    <section key={selectedProject.id} className="workspace-registry-detail-section" hidden={projectView !== 'directories'} aria-labelledby="workspace-directory-heading" tabIndex={0}>
                      <div className="workspace-registry-section-head">
                        <div>
                          <h4 id="workspace-directory-heading">项目目录</h4>
                          <span>{scopedPaths.length ? `${scopedPaths.length} 个${includeChildren ? '范围' : '直属'}目录` : selectedChildren.length ? '暂无直属目录，可查看子项目目录' : '还没有关联目录'}</span>
                        </div>
                        <button type="button" className="btn btn-primary" onClick={() => void bindPath(selectedProject)} disabled={loading || Boolean(busy)}>
                          <Plus size={14} />添加目录
                        </button>
                        {selectedPaths.length ? <button type="button" className="btn" disabled={loading || Boolean(busy) || !snapshot.revision} onClick={() => startOrganize()}>整理子项目</button> : null}
                      </div>
                      {scopedPaths.length ? (
                        <ul className="workspace-registry-detail-paths">
                          {scopedPaths.map(item => (
                            <li key={item.id}>
                              <FolderOpen size={16} aria-hidden="true" />
                              <div className="workspace-registry-detail-path-copy">
                                {includeChildren ? <button type="button" className="workspace-registry-project-link" onClick={() => selectProject(item.project_id)}>{projectName(item.project_id)}</button> : null}
                                <code className="workspace-registry-path" title={item.path}>{item.path}</code>
                                {item.role !== 'root' ? <span>{ROLE_LABELS[item.role] ?? item.role}</span> : null}
                              </div>
                              {item.confidence === 'inferred' ? <Pill kind="warn">待核对</Pill> : null}
                              <div className="actions-row workspace-registry-path-actions">
                                <button type="button" className="btn" disabled={Boolean(busy)} onClick={() => void accessDirectory(item.path, 'open')}><FolderOpen size={14} />打开目录</button>
                                <button type="button" className="btn" disabled={Boolean(busy)} onClick={() => void accessDirectory(item.path, 'copy')}><Copy size={14} />复制路径</button>
                                {item.project_id === selectedProject.id ? <button type="button" className="btn" disabled={loading || Boolean(busy) || !snapshot.revision} onClick={() => startOrganize(item.id)}>设为子项目</button> : null}
                                <button type="button" className="btn workspace-registry-unlink" disabled={loading || Boolean(busy)} onClick={() => void unbind(item)}>
                                  解除关联
                                </button>
                              </div>
                            </li>
                          ))}
                        </ul>
                      ) : (
                        <div className="workspace-registry-no-paths">
                          <FolderOpen size={18} aria-hidden="true" />
                          <span>添加本机目录后，Agent 才能根据目录识别这个项目。</span>
                        </div>
                      )}
                    </section>
                  </>
                ) : (
                  <div className="workspace-registry-detail-empty">从左侧选择一个项目。</div>
                )}
              </section>
            </>
          )}
        </div>
      ) : null}

      {tab === 'paths' && snapshot ? (
        <div className="workspace-registry-view workspace-registry-directory-view" id="workspace-registry-panel-paths" role="tabpanel" aria-label="目录清单">
          <header className="workspace-registry-list-head">
            <div>
              <h3>目录清单</h3>
              <p>按本机目录查看所属项目。解除关联不会删除目录或项目资料。</p>
            </div>
            <label className="workspace-registry-search workspace-registry-directory-search">
              <Search size={14} aria-hidden="true" />
              <span className="sr-only">搜索目录或项目</span>
              <input type="search" value={pathFilter} onChange={event => setPathFilter(event.target.value)} placeholder="搜索目录或项目" aria-label="搜索目录或项目" />
            </label>
          </header>
          {visiblePaths.length === 0 ? (
            <p className="workspace-registry-empty">{workspaces.length ? '没有匹配的目录。' : '还没有关联目录。可在项目详情中添加目录。'}</p>
          ) : (
            <div className="workspace-registry-table-wrap">
              <table className="table workspace-registry-table">
                <thead><tr><th>本机目录</th><th>所属项目</th><th>状态</th><th /></tr></thead>
                <tbody>
                  {visiblePaths.map(item => (
                    <tr key={item.id}>
                      <td className="workspace-registry-path"><code title={item.path}>{item.path}</code></td>
                      <td>
                        <button type="button" className="workspace-registry-project-link" onClick={() => { selectProject(item.project_id); setTab('projects'); }}>
                          {projectName(item.project_id)}
                        </button>
                      </td>
                      <td>{item.confidence === 'inferred' ? <Pill kind="warn">待核对</Pill> : <Pill kind="success">已确认</Pill>}</td>
                      <td><button type="button" className="btn workspace-registry-unlink" disabled={loading || Boolean(busy)} onClick={() => void unbind(item)}>解除关联</button></td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>
      ) : null}

      {tab === 'pending' && snapshot ? (
        <div className="workspace-registry-view workspace-registry-pending-view" id="workspace-registry-panel-pending" role="tabpanel" aria-label="待确认目录">
          <div className="workspace-registry-list-head">
            <div>
              <h3>待确认目录</h3>
              <p>Agent 遇到未关联的目录时，会把建议放在这里；确认后才会关联到项目。</p>
            </div>
          </div>
          {candidates.length === 0 ? (
            <p className="workspace-registry-empty">没有待确认目录。</p>
          ) : (
            <ul className="workspace-registry-candidates">
              {candidates.map(candidate => (
                <li key={candidate.path}>
                  <div className="workspace-registry-candidate-head">
                    <div><strong>{candidate.suggested_project || '未命名项目'}</strong><span>建议项目</span></div>
                    <Pill kind={candidate.confidence === 'exact' ? 'success' : 'warn'}>
                      {candidate.confidence === 'exact' ? '名称匹配' : '需要核对'}
                    </Pill>
                    <span className="muted">出现 {candidate.seen_count} 次</span>
                  </div>
                  <p className="workspace-registry-path">{candidate.path}</p>
                  <div className="actions-row workspace-registry-candidate-actions">
                    <button type="button" className="btn btn-primary" disabled={loading || Boolean(busy)} onClick={() => void acceptCandidate(candidate, 'create')}>
                      新建项目并添加目录
                    </button>
                    <select
                      className="input"
                      value={candidateProjects[candidate.path] ?? ''}
                      aria-label={`为目录 ${candidate.path} 选择所属项目`}
                      disabled={loading || Boolean(busy) || projects.length === 0}
                      onChange={event => {
                        const value = event.target.value;
                        setCandidateProjects(current => ({ ...current, [candidate.path]: value }));
                      }}
                    >
                      <option value="">选择所属项目…</option>
                      {projects.map(project => <option key={project.id} value={project.id}>{projectQualifiedName(projects, project)}</option>)}
                    </select>
                    <button type="button" className="btn" disabled={loading || Boolean(busy) || !candidateProjects[candidate.path]} onClick={() => void acceptCandidate(candidate, 'bind', candidateProjects[candidate.path])}>添加目录</button>
                    <button type="button" className="btn" disabled={loading || Boolean(busy)} onClick={() => void dismissCandidate(candidate)}>忽略</button>
                  </div>
                </li>
              ))}
            </ul>
          )}
        </div>
      ) : null}

      {organize ? <WorkspaceSubprojectsDialog {...organize} onClose={() => setOrganize(null)} onApplied={result => { setOrganize(null); setRollback(result); setNotice('子项目已保存'); void load(); }} /> : null}
      {edit ? (
        <ModalSurface className="workspace-registry-editor" titleId="workspace-editor-title" onClose={() => { if (!busy) setEdit(null); }} fallbackFocus={() => pageHeading.current}>
          <div className="workspace-registry-editor-head">
            <strong id="workspace-editor-title">{edit.id ? (edit.initialPath ? '完成添加目录' : '编辑项目') : edit.parentId ? '新建子项目' : '新建项目'}</strong>
            <button type="button" className="btn btn-icon" aria-label="关闭" disabled={loading || Boolean(busy)} onClick={() => setEdit(null)}><X size={16} /></button>
          </div>
          <form
            className="workspace-registry-form"
            onSubmit={event => { event.preventDefault(); void saveEdit(); }}
          >
            <label>
              <span id="workspace-project-name-label">项目别名</span>
              <input
                aria-labelledby="workspace-project-name-label"
                aria-describedby="workspace-project-name-hint"
                data-initial-focus
                required
                maxLength={40}
                value={edit.name}
                onChange={event => setEdit({ ...edit, name: event.target.value })}
              />
              <small id="workspace-project-name-hint">优先填写中文别名，原名仍可搜索。</small>
            </label>
            {!hasChinese(edit.name) && chineseProjectAliases({ aliases: splitList(edit.aliases) }).length > 1 ? (
              <label>
                <span>选择展示别名</span>
                <select value="" onChange={event => { if (event.target.value) setEdit({ ...edit, name: event.target.value }); }}>
                  <option value="">请选择一个中文别名</option>
                  {chineseProjectAliases({ aliases: splitList(edit.aliases) }).map(alias => <option key={alias} value={alias}>{alias}</option>)}
                </select>
              </label>
            ) : null}
            {!edit.id || edit.initialPath ? (
              <div className="workspace-registry-form-directory">
                <span>首个目录（可选）</span>
                <div className="workspace-registry-directory-picker">
                  <code title={edit.initialPath}>{edit.initialPath || '未选择目录'}</code>
                  {edit.initialPath ? <button type="button" className="btn btn-icon" aria-label="清除已选目录" title="清除已选目录" disabled={loading || Boolean(busy)} onClick={() => setEdit({ ...edit, initialPath: '' })}><X size={14} /></button> : null}
                  <button type="button" className="btn" disabled={loading || Boolean(busy)} onClick={() => void pickInitialDirectory()}>
                    <FolderOpen size={14} />{edit.initialPath ? '更换目录' : '选择目录'}
                  </button>
                </div>
                <small>保存项目时会同时关联此目录，也可以稍后添加。</small>
              </div>
            ) : null}
            <label>
              <span>其他名称（逗号分隔）</span>
              <input
                placeholder="work-log, 日志"
                value={edit.aliases}
                onChange={event => setEdit({ ...edit, aliases: event.target.value })}
              />
            </label>
            <label>
              <span>形态</span>
              <select value={edit.kind} onChange={event => setEdit({ ...edit, kind: event.target.value })}>
                {KIND_OPTIONS.map(option => (
                  <option key={option.value} value={option.value}>{option.label}</option>
                ))}
              </select>
            </label>
            <label>
              <span id="workspace-project-parent-label">上级项目</span>
              <select aria-labelledby="workspace-project-parent-label" value={edit.parentId} onChange={event => setEdit({ ...edit, parentId: event.target.value })}>
                <option value="">（无，作为根）</option>
                {parentOptions.map(project => (
                  <option key={project.id} value={project.id}>{projectQualifiedName(projects, project)}</option>
                ))}
              </select>
            </label>
            <label>
              <span>技术栈（逗号分隔）</span>
              <input
                placeholder="Go, JavaScript"
                value={edit.stack}
                onChange={event => setEdit({ ...edit, stack: event.target.value })}
              />
            </label>
            <label>
              <span>文档入口（逗号分隔路径）</span>
              <input
                placeholder="README.md, docs/design.md"
                value={edit.docs}
                onChange={event => setEdit({ ...edit, docs: event.target.value })}
              />
            </label>
            <label>
              <span>规则文件（逗号分隔路径）</span>
              <input
                placeholder="AGENTS.md"
                value={edit.rules}
                onChange={event => setEdit({ ...edit, rules: event.target.value })}
              />
            </label>
            <label>
              <span>备注</span>
              <textarea
                rows={3}
                value={edit.notes}
                onChange={event => setEdit({ ...edit, notes: event.target.value })}
              />
            </label>
            {error ? <WorkspaceFeedback issue={error} /> : null}
            <div className="actions-row workspace-registry-form-actions">
              {edit.id ? (
                <button
                  type="button"
                  className="btn btn-danger"
                  disabled={loading || Boolean(busy)}
                  onClick={() => {
                    const project = projects.find(item => item.id === edit.id);
                    if (project) void removeProject(project);
                  }}
                >
                  <Trash2 size={15} />删除
                </button>
              ) : null}
              <span className="workspace-registry-spacer" />
              <button type="button" className="btn" disabled={loading || Boolean(busy)} onClick={() => setEdit(null)}>取消</button>
              <button type="submit" className="btn btn-primary" disabled={loading || Boolean(busy)}>
                {busy === 'save' ? '保存中…' : edit.initialPath ? '保存并添加目录' : edit.id ? '保存修改' : '新建项目'}
              </button>
            </div>
          </form>
        </ModalSurface>
      ) : null}
    </section>
  );

  function renderTreeNode(node: WorkspaceRegistryTree) {
    const project = node.project;
    const childCount = projects.filter(item => item.parent_id === project.id).length;
    const hasChildren = node.children.length > 0;
    const expanded = Boolean(filter.trim()) || expandedProjectIds.has(project.id);
    const childrenId = `workspace-project-children-${project.id}`;
    return (
      <li key={project.id} className="workspace-registry-tree-item">
        <div className={`workspace-registry-node${selectedProjectId === project.id ? ' selected' : ''}`}>
          {hasChildren ? (
            <button
              type="button"
              className="workspace-registry-tree-toggle"
              aria-label={`${expanded ? '收起' : '展开'}${projectDisplayName(project)}的子项目`}
              aria-expanded={expanded}
              aria-controls={childrenId}
              onClick={() => setExpandedProjectIds(current => {
                const next = new Set(current);
                if (next.has(project.id)) next.delete(project.id);
                else next.add(project.id);
                return next;
              })}
            >
              {expanded ? <ChevronDown size={14} aria-hidden="true" /> : <ChevronRight size={14} aria-hidden="true" />}
            </button>
          ) : <span className="workspace-registry-tree-toggle-placeholder" aria-hidden="true" />}
          <button
            type="button"
            className="workspace-registry-node-select"
            aria-pressed={selectedProjectId === project.id}
            aria-label={childCount ? `${projectDisplayName(project)}，${childCount} 个子项目` : projectDisplayName(project)}
            title={[projectDisplayName(project), projectTechnicalName(project)].filter(Boolean).join(' · ')}
            onClick={() => selectProject(project.id)}
          >
            <FolderTree size={15} aria-hidden="true" />
            <span className="workspace-registry-node-copy">
              <strong>{projectDisplayName(project)}</strong>
            </span>
            {childCount ? <span className="workspace-registry-node-count" aria-hidden="true">{childCount}</span> : null}
          </button>
        </div>
        {hasChildren ? (
          <ul id={childrenId} className="workspace-registry-tree-children" hidden={!expanded}>
            {node.children.map(child => renderTreeNode(child))}
          </ul>
        ) : null}
      </li>
    );
  }
}
