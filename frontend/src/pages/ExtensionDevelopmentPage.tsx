import { useEffect, useMemo, useState } from 'react';
import { ArrowLeft, Blocks, CheckCircle2, CircleAlert, Clock3, Copy, FileText, FolderOpen, FolderPlus, GitBranch, Hammer, Inbox, MessageCircle, MoreHorizontal, Plus, Save, Search, Send, Trash2, UserPlus, Users, X } from 'lucide-react';
import { ActionMenu, ActionMenuItem } from '../components/ActionMenu';
import { BusyIndicator } from '../components/BusyIndicator';
import { EmptyState, PageHeader, Pill } from '../components/Common';
import { useConfirm } from '../components/ConfirmDialog';
import { ExtensionKindMark, extensionKindIcons } from '../components/ExtensionKindMark';
import { InstructionProjectionPanel } from '../components/InstructionProjectionPanel';
import { OperationPlanCard } from '../components/OperationPlanCard';
import { FUNCTIONAL_CATEGORIES } from '../data/categoryCatalog';
import { extensionKindLabels, extensionKindOrder } from '../data/extensionKinds';
import { agentApi } from '../services/agentApi';
import { compareVersions } from './marketCatalog';
import { formatCompactStamp, formatStamp } from '../timeFormat';
import type { AuthoringPluginDraft, AuthoringSkillDraft, AuthoringWorkflowDraft, CreateExtensionProjectInput, DistributionPreview, DistributionStateEntry, DistributionTarget, ExpertAuthoringDraft, ExpertSummary, ExtensionCollaboration, ExtensionCollaborationInvitation, ExtensionCollaboratorOption, ExtensionProject, ExtensionProjectKind, ExtensionProjectSourceInput, ExtensionRemoteProject, ExtensionSourceConfig, ExtensionWorkspaceEntry, ExtensionWorkspaceSettings, InstructionPackDraft, PluginCatalogItem, PluginSubmissionStatus, SkillSubmissionStatus } from '../services/agentApi';

type DraftRef =
  | { kind: 'plugin'; value: AuthoringPluginDraft }
  | { kind: 'skill'; value: AuthoringSkillDraft }
  | { kind: 'workflow'; value: AuthoringWorkflowDraft }
  | { kind: 'expert'; value: ExpertAuthoringDraft }
  | { kind: 'instruction'; value: InstructionPackDraft };

type SubmissionRef =
  | { kind: 'plugin'; value: PluginSubmissionStatus }
  | { kind: 'skill'; value: SkillSubmissionStatus }
  | { kind: 'workflow'; value: AuthoringWorkflowDraft };

type ProjectModel = {
  key: string;
  kind: ExtensionProjectKind;
  extensionId: string;
  name: string;
  description: string;
  local?: ExtensionProject;
  remote?: ExtensionRemoteProject;
  drafts: DraftRef[];
  submissions: SubmissionRef[];
};

type ExtensionBuildStage = 'building' | 'activating' | 'refreshing';

/// 左栏的一行：一个本机开发目录。`declared` 是登记表里的扩展数，只在项目列表
/// 整体读不到时兜底，免得一个明明有内容的目录显示成 0 个扩展。
type WorkspaceItem = {
  key: string;
  root: string;
  label: string;
  available: boolean;
  problem: string;
  declared: number;
  extensionCount: number;
  running: boolean;
};

/// 「工作台项目」不是目录工作区，但需要一个固定键才能和左栏选中状态共用一套逻辑。
const UNMANAGED_KEY = '__unmanaged__';
const WORKSPACE_MEMORY_KEY = 'himind.development.workspace';

function readPreferredWorkspace() {
  try { return normalizeFsPath(localStorage.getItem(WORKSPACE_MEMORY_KEY) || ''); }
  catch { return ''; }
}

/** Tauri 拒绝时抛出的通常是字符串；统一取出可读原因。 */
function failureText(error: unknown, fallback: string) {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === 'string' && error.trim()) return error.trim();
  return fallback;
}

type DevelopmentPageProps = {
  dashboardEnabled: boolean;
  /** 当前绑定的扩展工作区：只用来兜底（新建扩展的默认落点、没有登记表时的候选）。 */
  workspace: ExtensionWorkspaceSettings;
  /** 已配置的扩展源，其中的本地目录也要算进工作区列表。 */
  sources: ExtensionSourceConfig[];
  /** 登记过的开发工作区清单。这一页的左栏是它的直接映射。 */
  workspaces: ExtensionWorkspaceEntry[];
  projectsError: string;
  projects: ExtensionProject[];
  remoteProjects: ExtensionRemoteProject[];
  pluginDrafts: AuthoringPluginDraft[];
  skillDrafts: AuthoringSkillDraft[];
  workflowDrafts: AuthoringWorkflowDraft[];
  expertDrafts: ExpertAuthoringDraft[];
  instructionDrafts: InstructionPackDraft[];
  pluginSubmissions: PluginSubmissionStatus[];
  skillSubmissions: SkillSubmissionStatus[];
  workflowSubmissions: AuthoringWorkflowDraft[];
  experts: ExpertSummary[];
  activeExpert: string;
  onRefreshExperts: () => Promise<void> | void;
  onActivateExpert: (id: string, version: string) => Promise<void>;
  onNotify: (message: string, tone?: 'success' | 'error') => void;
  availablePlugins: PluginCatalogItem[];
  invitations: ExtensionCollaborationInvitation[];
  accountAuthorized: boolean;
  busyAction: string | null;
  /// 首次读取还没结束。冷启动时工作区与扩展都为空，直接渲染"0 个扩展 + 空态"
  /// 会让人以为数据丢了，所以这段时间只出加载态。
  loading: boolean;
  onRefresh: () => void;
  /** `parentDir` 是新建项目要落进的工作区目录，由弹窗里的工作区选择决定。 */
  onCreate: (input: CreateExtensionProjectInput, parentDir: string) => Promise<ExtensionProject>;
  onOpenProject: () => Promise<void>;
  onAssociateProject: (project: ExtensionRemoteProject) => Promise<void>;
  onBuild: (projectId: string, onProgress?: (stage: ExtensionBuildStage) => void) => Promise<void>;
  onDevelopWithAi: (project: ExtensionProject) => void;
  /// 「用 AI 开发」：把某个扩展工作区交给 HiMind AI，直接开一条绑定该目录的会话。
  /// 与来源管理里的「设为当前工作区并打开扩展开发」是两件事，故不与 onDevelopWorkspace 同名。
  onDevelopWorkspaceWithAi: (root: string) => void;
  onSubmit: (kind: ExtensionProjectKind, extensionId: string, version: string) => Promise<void>;
  /** 项目规则的收敛动作：把已确认的规则版本发布到本机规则库。 */
  onPublishInstruction: (extensionId: string, version: string) => Promise<void>;
  onOpenFolder: (path: string) => void;
  /** 选一个本机目录加进工作区列表。 */
  onAddWorkspace: () => Promise<void>;
  /** 把工作区从列表里移除，只动登记，不删目录内容。 */
  onRemoveWorkspace: (root: string) => Promise<void>;
  onRemove: (projectId: string) => Promise<void>;
  onUpdateSource: (projectId: string, input: ExtensionProjectSourceInput, syncRemote: boolean) => Promise<void>;
  /** 设置项目级分发目标；传 null 表示继承分发单元默认。 */
  onSetDistributionTargets: (kind: ExtensionProjectKind, extensionId: string, targets: DistributionTarget[] | null) => Promise<void>;
  /** 按生效目标发布（先 GitHub 后工作台）。 */
  onPublishDistribution: (kind: ExtensionProjectKind, extensionId: string, version: string) => Promise<void>;
  onLoadCollaboration: (productKey: string) => Promise<ExtensionCollaboration>;
  onSearchCollaborators: (productKey: string, query: string) => Promise<ExtensionCollaboratorOption[]>;
  onInviteCollaborator: (productKey: string, userId: string) => Promise<unknown>;
  onRemoveCollaborator: (productKey: string, userId: string) => Promise<unknown>;
  onRespondInvitation: (invitationId: string, action: 'accept' | 'decline') => Promise<void>;
};

export function ExtensionDevelopmentPage(props: DevelopmentPageProps) {
  const confirm = useConfirm();
  const [query, setQuery] = useState('');
  const [kindFilter, setKindFilter] = useState<'all' | ExtensionProjectKind>('all');
  const [selectedKey, setSelectedKey] = useState('');
  const [selectedWorkspace, setSelectedWorkspace] = useState('');
  const [detailOpen, setDetailOpen] = useState(false);
  const [createOpen, setCreateOpen] = useState(false);
  const [removeProject, setRemoveProject] = useState<ExtensionProject | null>(null);
  const [runningSessions, setRunningSessions] = useState<Set<string>>(() => new Set());
  const [actionError, setActionError] = useState('');
  /// 上一次新建扩展选的工作区。它是「上次选择」，不是「当前工作区」：
  /// 当前工作区由左栏选中决定，两者混用会让左栏点击变得有副作用。
  const [preferredWorkspace, setPreferredWorkspace] = useState(readPreferredWorkspace);
  const models = useMemo(() => buildProjectModels(props), [props.projects, props.remoteProjects, props.pluginDrafts, props.skillDrafts, props.workflowDrafts, props.expertDrafts, props.pluginSubmissions, props.skillSubmissions, props.workflowSubmissions]);
  // 工作区 = 本机的一个开发目录，是这一页的一等实体：左栏选中它，右栏就只属于它。
  //
  // 数据优先取登记表——它接受任意目录（不要求有聚合清单），也能如实报告
  // 「登记过但目录当前不可用」；再并入本地扩展源与当前绑定目录，最后才按
  // 项目所在目录兜底。之所以不把 project.workspace_path 当工作区，是因为它
  // 指向的是「这一个扩展的目录」，同一个仓库里的 N 个扩展会被切成 N 个工作区。
  const workspaceItems = useMemo(() => {
    const list: WorkspaceItem[] = [];
    const seen = new Set<string>();
    const push = (root: string, label: string, available: boolean, problem: string, declared: number) => {
      const key = normalizeFsPath(root);
      if (!key || seen.has(key)) return;
      seen.add(key);
      list.push({ key, root, label: label.trim() || workspaceLabelFromRoot(root), available, problem, declared, extensionCount: 0, running: false });
    };
    for (const entry of props.workspaces) {
      push(
        entry.root,
        entry.name,
        entry.available,
        !entry.available ? '目录当前不可用' : entry.has_catalog && !entry.valid ? (entry.error || '聚合清单无法读取') : '',
        entry.extension_count,
      );
    }
    for (const source of props.sources) {
      if (source.kind === 'local') push(source.repository, sourceName(source), true, '', 0);
    }
    push(
      props.workspace.root,
      workspaceLabelFromRoot(props.workspace.root),
      props.workspace.valid,
      props.workspace.valid ? '' : (props.workspace.error || '目录当前不可用'),
      props.workspace.extension_count,
    );
    // 登记表、来源、绑定目录都读不到时，至少按项目所在目录列出来，页面不至于空掉。
    if (!list.length) for (const project of models) push(projectWorkspaceRoot(project), '', true, '', 0);
    const counts = new Map<string, number>();
    for (const project of models) {
      if (!project.local) continue;
      const key = workspaceKeyFor(project, list);
      if (key) counts.set(key, (counts.get(key) || 0) + 1);
    }
    for (const item of list) {
      // 扩展计数以列表实际显示的项目为准；项目列表整体读取失败时才退回登记表里的数字，
      // 否则一个明明有 12 个扩展的工作区会显示成 0 个。
      item.extensionCount = counts.get(item.key) || (item.declared > 0 && !item.problem ? item.declared : 0);
      item.running = runningSessions.has(item.key);
    }
    return list.sort((left, right) => left.label.localeCompare(right.label, 'zh-CN'));
  }, [models, props.sources, props.workspace.error, props.workspace.extension_count, props.workspace.root, props.workspace.valid, props.workspaces, runningSessions]);
  const busy = Boolean(props.busyAction) || props.loading;
  // 还没有本地目录的远端项目（作者在工作台上参与过的扩展）单独成一项：
  // 它们的下一步是「关联本地目录」，塞进任何一个已存在的工作区都不对。
  const unassigned = useMemo(() => models.filter(project => !workspaceKeyFor(project, workspaceItems)), [models, workspaceItems]);
  // 没有手动选过时：优先有扩展的那个工作区，其次看后端当前绑定的目录
  // （「添加工作区」和「在扩展开发中打开」都会更新绑定，于是刚加完就落在它上面）。
  const boundKey = workspaceItems.find(item => item.key === normalizeFsPath(props.workspace.root))?.key || '';
  const activeKey = workspaceItems.some(item => item.key === selectedWorkspace)
    ? selectedWorkspace
    : (workspaceItems.find(item => item.extensionCount > 0)?.key || boundKey || workspaceItems[0]?.key || '');
  const activeItem = workspaceItems.find(item => item.key === activeKey) || null;
  const showingUnmanaged = activeKey === UNMANAGED_KEY;
  const workspaceProjects = useMemo(
    () => (showingUnmanaged ? unassigned : models.filter(project => workspaceKeyFor(project, workspaceItems) === activeKey)),
    [activeKey, models, showingUnmanaged, unassigned, workspaceItems],
  );
  const visible = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    return workspaceProjects.filter(project => {
      if (kindFilter !== 'all' && project.kind !== kindFilter) return false;
      return !normalized || `${project.name} ${project.extensionId} ${project.description}`.toLowerCase().includes(normalized);
    });
  }, [kindFilter, query, workspaceProjects]);
  const selected = visible.find(project => project.key === selectedKey) || visible[0] || null;
  const hasRail = workspaceItems.length > 1 || unassigned.length > 0;
  // 新建扩展默认落在上次选过的目录：连着建几个扩展时不必每次重挑。
  const createRoot = workspaceItems.some(item => item.key === preferredWorkspace) ? preferredWorkspace : activeKey;
  const workspaceChoices = useMemo(() => {
    const list = workspaceItems.map(item => ({ root: item.root, label: item.label }));
    const index = list.findIndex(item => normalizeFsPath(item.root) === createRoot);
    if (index > 0) list.unshift(list.splice(index, 1)[0]);
    return list;
  }, [createRoot, workspaceItems]);
  const headPath = showingUnmanaged ? '' : activeItem?.root || '';
  const headProblem = showingUnmanaged ? '' : activeItem?.problem || '';
  const headMeta = showingUnmanaged
    ? `${unassigned.length} 个扩展 · 未关联本地目录`
    : activeItem
      ? `${headProblem ? `${headProblem} · ` : ''}${activeItem.root} · ${activeItem.extensionCount} 个扩展`
      : props.loading
        ? '正在读取工作区和扩展'
        : '添加本机目录后开始开发';

  // 「有没有 AI 会话在这个目录里跑」是工作区行最值得显示的状态，进入页面时取一次即可：
  // 会话的实时状态在 AI 会话页里已经是常驻信息，这里不重复轮询。
  useEffect(() => {
    let disposed = false;
    void agentApi.listBuiltinAiSessions()
      .then(items => { if (!disposed) setRunningSessions(new Set(items.map(item => normalizeFsPath(item.workspace_root)))); })
      .catch(() => undefined);
    return () => { disposed = true; };
  }, []);

  useEffect(() => {
    if (selected && selected.key !== selectedKey) setSelectedKey(selected.key);
  }, [selected, selectedKey]);

  const addWorkspace = async () => {
    setActionError('');
    try { await props.onAddWorkspace(); }
    catch (reason) { setActionError(failureText(reason, '添加工作区失败')); }
  };
  const removeWorkspace = async (root: string, label: string) => {
    const accepted = await confirm({
      title: `从扩展开发里移除「${label}」？`,
      description: '只移除这条登记，目录里的文件不会动。',
      confirmText: '移除',
    });
    if (!accepted) return;
    setActionError('');
    try { await props.onRemoveWorkspace(root); }
    catch (reason) { setActionError(failureText(reason, '移除工作区失败')); }
  };
  const copyPath = async (path: string) => {
    setActionError('');
    try { await navigator.clipboard.writeText(path); }
    catch { setActionError('复制路径失败，请在文件资源管理器里复制。'); }
  };

  const materializeInstruction = async (workspaceRoot: string, draft: InstructionPackDraft) => {
    await agentApi.materializeInstructionProject(workspaceRoot, draft.manifest.id, draft.manifest.version);
    props.onRefresh();
  };

  return <div className="development-page">
      <PageHeader title="扩展开发" description={props.dashboardEnabled ? '创建、开发、测试并分发自己的 Agent 能力。' : '创建、开发和测试自己的 Agent 能力。'} actions={<>
      {/* 只有一个工作区时左栏会收起，添加入口就移到页头，免得没有地方开始。 */}
      {hasRail ? null : <button className="btn" title="添加本机开发目录" disabled={busy} onClick={() => void addWorkspace()}><FolderPlus size={15} />添加工作区</button>}
      {/* 页头动作给出文字标签，主操作仍是「新建扩展」一个；工作区和扩展列表在进入页面时读取。 */}
      <button className="btn" title="选择 HiMind 项目或扩展聚合仓库" disabled={busy} onClick={() => void props.onOpenProject()}><FolderOpen size={15} />添加已有项目</button>
      <button className="btn btn-primary" title="新建扩展" disabled={busy} onClick={() => setCreateOpen(true)}><Plus size={15} />新建扩展</button>
    </>} />
    {props.dashboardEnabled && props.invitations.length ? <InvitationInbox invitations={props.invitations} busyAction={props.busyAction} onRespond={props.onRespondInvitation} /> : null}
    {props.projectsError ? <div className="development-project-error"><CircleAlert size={16} /><div><strong>项目列表读取失败</strong><span>{props.projectsError}</span></div><button className="btn" onClick={props.onRefresh}>重试</button></div> : null}
    {actionError ? <div className="development-project-error"><CircleAlert size={16} /><div><strong>操作未完成</strong><span>{actionError}</span></div><button className="btn" onClick={() => setActionError('')}>关闭</button></div> : null}
    {props.loading ? <div className="development-body development-body-loading" role="status"><BusyIndicator size={16} /><span>正在读取工作区和扩展</span></div> : <div className={`development-body ${hasRail ? 'has-rail' : ''}`}>
      {hasRail ? <aside className="development-rail" aria-label="开发工作区">
        <div className="development-rail-head"><strong>工作区</strong><button className="btn btn-icon" title="添加工作区" aria-label="添加工作区" disabled={busy} onClick={() => void addWorkspace()}><Plus size={15} /></button></div>
        <div className="development-rail-body">
          {workspaceItems.map(item => <WorkspaceRow
            key={item.key}
            item={item}
            active={!showingUnmanaged && item.key === activeKey}
            busy={busy}
            onSelect={() => { setSelectedWorkspace(item.key); setDetailOpen(false); }}
            onOpenFolder={() => props.onOpenFolder(item.root)}
            onCopyPath={() => void copyPath(item.root)}
            onRemove={() => void removeWorkspace(item.root, item.label)}
          />)}
          {/* 未关联本地目录的远端项目不是工作区，但需要同一个入口才能被看见和关联。 */}
          {unassigned.length ? <div className={`development-rail-item ${showingUnmanaged ? 'active' : ''}`}>
            <button type="button" className="development-rail-select" aria-current={showingUnmanaged} onClick={() => { setSelectedWorkspace(UNMANAGED_KEY); setDetailOpen(false); }}>
              <span className="development-rail-copy"><strong><span>工作台项目</span></strong><small>{unassigned.length} 个扩展</small></span>
            </button>
          </div> : null}
        </div>
      </aside> : null}
      <div className="development-main">
        <header className="development-workspace-head">
          <div className="development-workspace-copy">
            <strong>{props.loading ? '正在读取…' : showingUnmanaged ? '工作台项目' : activeItem?.label || '还没有工作区'}</strong>
            <small className={headProblem ? 'is-problem' : undefined} title={headPath || undefined}>{headMeta}</small>
          </div>
          {!showingUnmanaged && activeItem ? <div className="development-workspace-actions">
            {/* 这一层是「整个仓库」，下面详情里是「单个扩展」。两个都叫「用 AI 开发」时
                会话落在哪个目录看不出区别，所以这里写清范围，并按次级动作处理。 */}
            <button className="btn development-workspace-develop" title={activeItem.available ? `在 ${activeItem.root} 里开一条 AI 会话` : '目录当前不可用，先在文件资源管理器里恢复它'} disabled={busy || !activeItem.available} onClick={() => props.onDevelopWorkspaceWithAi(activeItem.root)}><MessageCircle size={15} />在工作区开发</button>
            {/* 有左栏时，每个工作区的操作在它自己那一行；没有左栏时只能放到这里。 */}
            {hasRail ? null : <ActionMenu variant="icon" icon={<MoreHorizontal size={16} />} title="工作区操作">
              {close => <>
                <ActionMenuItem icon={<FolderOpen size={15} />} label="打开目录" onClick={() => { close(); props.onOpenFolder(activeItem.root); }} />
                <ActionMenuItem icon={<Copy size={15} />} label="复制路径" onClick={() => { close(); void copyPath(activeItem.root); }} />
                <ActionMenuItem icon={<Trash2 size={15} />} label="从列表移除" danger disabled={busy} onClick={() => { close(); void removeWorkspace(activeItem.root, activeItem.label); }} />
              </>}
            </ActionMenu>}
          </div> : null}
        </header>
        {/* 规则库是工作区级的：预检、确认、发布和落地都作用在本机规则库，不绑某个扩展项目。 */}
        {activeItem && !showingUnmanaged ? <InstructionProjectionPanel workspaceRoot={activeItem.root} onMaterializeToWorkspace={(draft) => materializeInstruction(activeItem.root, draft)} /> : null}
        {activeItem && !showingUnmanaged ? <div className="development-toolbar">
          <div className="plugin-tabs" role="tablist" aria-label="扩展类型">
            <button className={kindFilter === 'all' ? 'active' : ''} onClick={() => setKindFilter('all')}>全部 <span>{workspaceProjects.length}</span></button>
            {/* 类型页签和市场、我的能力用同一套图标，同一类东西在哪儿都长一个样。 */}
            {extensionKindOrder.map(kind => {
              const KindIcon = extensionKindIcons[kind];
              return <button key={kind} className={kindFilter === kind ? 'active' : ''} onClick={() => setKindFilter(kind)}><KindIcon size={14} />{extensionKindLabels[kind]} <span>{workspaceProjects.filter(item => item.kind === kind).length}</span></button>;
            })}
          </div>
          <label className="development-search"><Search size={15} /><input value={query} onChange={event => setQuery(event.target.value)} placeholder="搜索扩展" /></label>
        </div> : null}
        {!activeItem && !showingUnmanaged ? <div className="development-workspace-empty">
          <EmptyState icon={FolderPlus} title="还没有开发工作区" text="添加一个本机目录，就能在里面开发扩展。" />
          <div className="development-workspace-empty-actions"><button className="btn btn-primary" disabled={busy} onClick={() => void addWorkspace()}><FolderPlus size={15} />添加工作区</button></div>
        </div> : !workspaceProjects.length ? <div className="development-workspace-empty">
          <EmptyState icon={Blocks} title={showingUnmanaged ? '没有未关联的扩展' : '这个工作区还没有扩展'} text={showingUnmanaged ? '工作台上还没有「未关联本地目录」的扩展。' : '新建一个扩展，或者让 AI 直接在这个目录里开始开发。'} />
          {showingUnmanaged || !activeItem ? null : <div className="development-workspace-empty-actions">
            <button className="btn" disabled={busy} onClick={() => setCreateOpen(true)}><Plus size={15} />新建扩展</button>
            <button className="btn" disabled={busy} onClick={() => props.onDevelopWorkspaceWithAi(activeItem.root)}><MessageCircle size={15} />在工作区开发</button>
          </div>}
        </div> : <section className={`development-workspace compact-master-detail ${detailOpen ? 'detail-open' : ''}`}>
          <aside className="development-project-list">
            <div className="development-list-heading"><strong>扩展</strong><span>{visible.length}</span></div>
            <div className="development-list-body">
              {visible.map(project => <ProjectListItem key={project.key} project={project} dashboardEnabled={props.dashboardEnabled} busyAction={props.busyAction} selected={project.key === selected?.key} onSelect={key => { setSelectedKey(key); setDetailOpen(true); }} />)}
              {!visible.length ? <EmptyState icon={Blocks} title="没有匹配的扩展" text="换个关键词或类型再试。" /> : null}
            </div>
          </aside>
          <main className="development-project-detail">
            <button className="workspace-back development-back" onClick={() => setDetailOpen(false)}><ArrowLeft size={15} />返回扩展列表</button>
            {selected ? <ProjectDetail key={selected.key} project={selected} dashboardEnabled={props.dashboardEnabled} accountAuthorized={props.accountAuthorized} availablePlugins={props.availablePlugins} busyAction={props.busyAction} onOpenProject={props.onOpenProject} onAssociateProject={props.onAssociateProject} onBuild={props.onBuild} onDevelopWithAi={props.onDevelopWithAi} onSubmit={props.onSubmit} onPublishInstruction={props.onPublishInstruction} onOpenFolder={props.onOpenFolder} onRequestRemove={setRemoveProject} onUpdateSource={props.onUpdateSource} onSetDistributionTargets={props.onSetDistributionTargets} onPublishDistribution={props.onPublishDistribution} onLoadCollaboration={props.onLoadCollaboration} onSearchCollaborators={props.onSearchCollaborators} onInviteCollaborator={props.onInviteCollaborator} onRemoveCollaborator={props.onRemoveCollaborator} /> : <EmptyState icon={Hammer} title="选择一个扩展" text={props.dashboardEnabled ? '查看本地项目、构建和发布进度。' : '查看本地项目、构建和版本。'} />}
          </main>
        </section>}
      </div>
    </div>}
    {createOpen ? <CreateProjectDialog busy={busy} workspaces={workspaceChoices} defaultWorkspace={workspaceChoices[0]?.root || ''} onClose={() => setCreateOpen(false)} onCreate={async (input, parentDir) => { try { const project = await props.onCreate(input, parentDir); const remembered = normalizeFsPath(parentDir); setPreferredWorkspace(remembered); try { localStorage.setItem(WORKSPACE_MEMORY_KEY, remembered); } catch { /* 记不住就每次重选，不影响创建。 */ } setSelectedWorkspace(remembered); setQuery(''); setKindFilter('all'); setSelectedKey(`${project.kind}:${project.extension_id}`); setDetailOpen(true); setCreateOpen(false); return project; } catch (error) { /* The parent keeps the dialog open and shows the error. */ throw error; } }} /> : null}
    {removeProject ? <ConfirmRemoveDialog project={removeProject} dashboardEnabled={props.dashboardEnabled} busy={busy} onClose={() => setRemoveProject(null)} onConfirm={async () => { await props.onRemove(removeProject.id); setRemoveProject(null); }} /> : null}
  </div>;
}

/// 左栏的一行工作区：点它选中，选中后右栏整块都属于它。
/// 「用 AI 开发」这类主按钮不放在行里：一栏堆满按钮既压花列表，也会让
/// 「选中工作区」这个主动作变得不像主动作；行的 ⋯ 只装这一行的管理动作。
function WorkspaceRow({ item, active, busy, onSelect, onOpenFolder, onCopyPath, onRemove }: {
  item: WorkspaceItem;
  active: boolean;
  busy: boolean;
  onSelect: () => void;
  onOpenFolder: () => void;
  onCopyPath: () => void;
  onRemove: () => void;
}) {
  return <div className={`development-rail-item ${active ? 'active' : ''}`}>
    <button type="button" className="development-rail-select" aria-current={active} title={item.root} onClick={onSelect}>
      <span className="development-rail-copy">
        <strong>{item.running ? <span className="development-rail-live" title="这个目录里有 AI 会话在运行" /> : null}<span>{item.label}</span></strong>
        <small className={item.problem ? 'is-problem' : undefined}>{item.problem || `${item.extensionCount} 个扩展`}</small>
      </span>
    </button>
    <div className="development-rail-actions">
      <ActionMenu variant="icon" icon={<MoreHorizontal size={16} />} title={`${item.label} · 更多操作`}>
        {close => <>
          <ActionMenuItem icon={<FolderOpen size={15} />} label="打开目录" onClick={() => { close(); onOpenFolder(); }} />
          <ActionMenuItem icon={<Copy size={15} />} label="复制路径" onClick={() => { close(); onCopyPath(); }} />
          <ActionMenuItem icon={<Trash2 size={15} />} label="从列表移除" danger disabled={busy} onClick={() => { close(); onRemove(); }} />
        </>}
      </ActionMenu>
    </div>
  </div>;
}

function InvitationInbox({ invitations, busyAction, onRespond }: { invitations: ExtensionCollaborationInvitation[]; busyAction: string | null; onRespond: DevelopmentPageProps['onRespondInvitation'] }) {
  const [error, setError] = useState('');
  const respond = async (id: string, action: 'accept' | 'decline') => {
    setError('');
    try { await onRespond(id, action); } catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)); }
  };
  return <section className="development-invitations" aria-label="协作邀请">
    <div className="development-invitations-head"><Inbox size={16} /><strong>协作邀请</strong><span>{invitations.length}</span></div>
    <div className="development-invitation-list">{invitations.map(item => <article key={item.id}>
      <div><strong>{item.product_name}</strong><small>{extensionKindLabel(item.product_type)} · {roleLabel(item.role)}{item.invited_by_name ? ` · ${item.invited_by_name}` : ''}</small></div>
      <div><button className="btn" disabled={Boolean(busyAction)} onClick={() => void respond(item.id, 'decline')}>拒绝</button><button className="btn btn-primary" disabled={Boolean(busyAction)} onClick={() => void respond(item.id, 'accept')}>接受</button></div>
    </article>)}</div>
    {error ? <p className="development-inline-error">{error}</p> : null}
  </section>;
}

function ProjectListItem({ project, dashboardEnabled, busyAction, selected, onSelect }: { project: ProjectModel; dashboardEnabled: boolean; busyAction: string | null; selected: boolean; onSelect: (key: string) => void }) {
  const draft = currentDraft(project);
  const active = activeSubmission(project);
  const state = projectState(project, dashboardEnabled, draft, active);
  const building = Boolean(project.local && busyAction === `build:${project.local.id}`);
  return <button className={`development-project-item ${selected ? 'selected' : ''}`} onClick={() => onSelect(project.key)}>
    <ExtensionKindMark kind={project.kind} size={16} label={extensionKindLabels[project.kind]} />
    <span className="development-project-copy"><strong>{project.name}</strong><small title={`${extensionKindLabels[project.kind]} · ${projectSourceLabel(project)}${project.local ? ` · v${project.local.version}` : ''}`}>{listMeta(project)}</small>{active ? <small>{submissionStatus(active).label} · v{active.value.version}</small> : null}</span>
    <span className={`skill-state-label ${building ? 'warn' : state.tone}`}>{building ? '构建中' : state.label}</span>
  </button>;
}

function ProjectDetail({ project, dashboardEnabled, accountAuthorized, availablePlugins, busyAction, onOpenProject, onAssociateProject, onBuild, onDevelopWithAi, onSubmit, onPublishInstruction, onOpenFolder, onRequestRemove, onUpdateSource, onSetDistributionTargets, onPublishDistribution, onLoadCollaboration, onSearchCollaborators, onInviteCollaborator, onRemoveCollaborator }: {
  project: ProjectModel;
  dashboardEnabled: boolean;
  accountAuthorized: boolean;
  availablePlugins: PluginCatalogItem[];
  busyAction: string | null;
  onOpenProject: () => Promise<void>;
  onAssociateProject: DevelopmentPageProps['onAssociateProject'];
  onBuild: DevelopmentPageProps['onBuild'];
  onDevelopWithAi: DevelopmentPageProps['onDevelopWithAi'];
  onSubmit: DevelopmentPageProps['onSubmit'];
  onPublishInstruction: DevelopmentPageProps['onPublishInstruction'];
  onOpenFolder: (path: string) => void;
  onRequestRemove: (project: ExtensionProject) => void;
  onUpdateSource: DevelopmentPageProps['onUpdateSource'];
  onSetDistributionTargets: DevelopmentPageProps['onSetDistributionTargets'];
  onPublishDistribution: DevelopmentPageProps['onPublishDistribution'];
  onLoadCollaboration: DevelopmentPageProps['onLoadCollaboration'];
  onSearchCollaborators: DevelopmentPageProps['onSearchCollaborators'];
  onInviteCollaborator: DevelopmentPageProps['onInviteCollaborator'];
  onRemoveCollaborator: DevelopmentPageProps['onRemoveCollaborator'];
}) {
  const [tab, setTab] = useState<'overview' | 'release' | 'collaboration' | 'settings'>('overview');
  const [buildStage, setBuildStage] = useState<ExtensionBuildStage | null>(null);
  const draft = currentDraft(project);
  const active = activeSubmission(project);
  const state = projectState(project, dashboardEnabled, draft, active);
  const version = draft ? draftVersion(draft) : project.local?.version || active?.value.version || '--';
  const busy = Boolean(busyAction);
  const dependencies = draftDependencies(draft, availablePlugins);
  const buildProject = async () => {
    if (!project.local) return;
    setBuildStage('building');
    try { await onBuild(project.local.id, setBuildStage); }
    finally { setBuildStage(null); }
  };

  return <>
      <header className="development-detail-header">
      <div className="development-detail-title"><ExtensionKindMark kind={project.kind} size={16} /><div><div><h3>{project.name}</h3><Pill kind={state.tone}>{state.label}</Pill></div><small title={projectSourceIdentity(project)}>{extensionKindLabels[project.kind]} · {projectSourceLabel(project)} · v{version}</small></div></div>
      <div className="development-detail-actions">
        {/* 这三个动作是这个页面上最常用的操作，图标 + 文字一起给，避免「点之前不知道是什么」。 */}
        {project.local?.workspace_available ? <button className="btn btn-primary" title="在这个项目目录里开一条 AI 会话" disabled={busy} onClick={() => onDevelopWithAi(project.local!)}><MessageCircle size={15} />用 AI 开发</button> : <button className="btn btn-primary" title="选择协作项目的本地目录" disabled={busy} onClick={() => void (project.remote ? onAssociateProject(project.remote) : onOpenProject())}><FolderOpen size={15} />关联本地项目</button>}
        {project.local?.workspace_available ? <button className="btn" title="在文件管理器里打开项目目录" onClick={() => onOpenFolder(project.local!.workspace_path)}><FolderOpen size={15} />打开目录</button> : null}
        {project.local?.workspace_available ? <button className="btn" title={buildStage ? '正在构建项目' : '构建并生成测试制品'} disabled={busy} onClick={() => void buildProject()}>{buildStage ? <BusyIndicator size={15} /> : <Hammer size={15} />}构建</button> : null}
      </div>
    </header>
    <div className="extension-detail-tabs development-detail-tabs" role="tablist">
      {([['overview', '概览'], ['release', dashboardEnabled ? '发布' : '版本'], ...(dashboardEnabled ? [['collaboration', '协作者'] as const] : []), ['settings', '设置']] as const).map(item => <button key={item[0]} className={tab === item[0] ? 'active' : ''} onClick={() => setTab(item[0])}>{item[1]}{item[0] === 'release' && active && ['changes_requested', 'rejected'].includes(active.value.status || '') ? <span className="tab-alert" /> : null}</button>)}
    </div>
    {buildStage ? <BuildProgress kind={project.kind} stage={buildStage} /> : null}
    <div className="development-detail-body">
      {tab === 'overview' ? <>
        {!project.local?.workspace_available ? <div className="development-notice"><CircleAlert size={16} /><div><strong>未关联本地项目</strong><span>选择包含插件、技能或工作流清单的项目目录。</span></div></div> : null}
        {project.description || draft ? <section className="development-section"><h4>项目说明</h4><p>{project.description || (draft ? draftDescription(draft) : '')}</p></section> : null}
        {/* 源码目录拿不到时，本机残留的构建记录与当前代码已经对不上，展示它只会
            让人以为这个项目还有一份可用的测试制品；等内容源恢复后再显示。 */}
        {project.local?.workspace_available ? <section className="development-section"><h4>最近构建</h4>{draft ? <BuildSummary project={project} dashboardEnabled={dashboardEnabled} draft={draft} active={active} busy={busy} onSubmit={onSubmit} onPublishInstruction={onPublishInstruction} /> : <div className="development-empty-line"><span>尚未构建</span></div>}</section> : null}
        <section className="development-section"><h4>依赖</h4>{dependencies.length ? <div className="development-dependency-list">{dependencies.map(item => <div key={item.id}><strong>{item.name}</strong><small>{item.required ? '必需' : '可选'}{item.version ? ` · ${item.version}` : ''}</small></div>)}</div> : <p className="muted">无依赖</p>}</section>
      </> : null}
      {tab === 'release' ? <ReleasePanel project={project} dashboardEnabled={dashboardEnabled} draft={draft} active={active} busy={busy} onSubmit={onSubmit} onPublish={onPublishDistribution} /> : null}
      {tab === 'collaboration' && dashboardEnabled ? <CollaborationPanel project={project} accountAuthorized={accountAuthorized} busyAction={busyAction} onLoad={onLoadCollaboration} onSearch={onSearchCollaborators} onInvite={onInviteCollaborator} onRemove={onRemoveCollaborator} /> : null}
      {tab === 'settings' ? <SettingsPanel project={project} dashboardEnabled={dashboardEnabled} busy={busy} onOpenFolder={onOpenFolder} onRequestRemove={onRequestRemove} onUpdateSource={onUpdateSource} onSetDistributionTargets={onSetDistributionTargets} /> : null}
    </div>
  </>;
}

function BuildProgress({ kind, stage }: { kind: ExtensionProjectKind; stage: ExtensionBuildStage }) {
  const stages: Array<{ id: ExtensionBuildStage; label: string }> = [
    { id: 'building', label: '构建' },
    { id: 'activating', label: '启用' },
    { id: 'refreshing', label: '刷新' },
  ];
  const current = stages.findIndex(item => item.id === stage);
  const message = stage === 'building'
    ? '正在检查项目并生成版本'
    : stage === 'activating'
      ? kind === 'instruction'
        ? '正在写入本机规则库'
        : (kind === 'plugin' ? '正在启用插件' : kind === 'workflow' ? '正在检查工作流' : '正在同步技能')
      : '正在更新可用状态';
  return <div className="development-build-progress" role="status" aria-live="polite">
    <div className="development-build-progress-copy"><BusyIndicator size={16} /><div><strong>{message}</strong><small>请不要关闭应用</small></div></div>
    <ol>{stages.map((item, index) => <li key={item.id} className={index < current ? 'complete' : index === current ? 'active' : ''}><span>{index < current ? <CheckCircle2 size={12} /> : index + 1}</span>{item.label}</li>)}</ol>
  </div>;
}

function CollaborationPanel({ project, accountAuthorized, busyAction, onLoad, onSearch, onInvite, onRemove }: {
  project: ProjectModel;
  accountAuthorized: boolean;
  busyAction: string | null;
  onLoad: DevelopmentPageProps['onLoadCollaboration'];
  onSearch: DevelopmentPageProps['onSearchCollaborators'];
  onInvite: DevelopmentPageProps['onInviteCollaborator'];
  onRemove: DevelopmentPageProps['onRemoveCollaborator'];
}) {
  const confirm = useConfirm();
  const [collaboration, setCollaboration] = useState<ExtensionCollaboration | null>(null);
  const [options, setOptions] = useState<ExtensionCollaboratorOption[]>([]);
  const [query, setQuery] = useState('');
  const [selectedUser, setSelectedUser] = useState('');
  const [loading, setLoading] = useState(true);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState('');
  const load = async () => {
    if (!accountAuthorized) {
      setCollaboration(null); setOptions([]); setError(''); setLoading(false);
      return;
    }
    setLoading(true); setError('');
    try {
      const value = await onLoad(project.extensionId);
      setCollaboration(value);
      if (value.can_manage && value.registered && query.trim()) setOptions(await onSearch(project.extensionId, query));
    } catch (reason) { setError(collaborationErrorMessage(reason)); }
    finally { setLoading(false); }
  };
  useEffect(() => { void load(); }, [project.extensionId, accountAuthorized]);
  const search = async () => {
    setError('');
    if (!query.trim()) { setOptions([]); return; }
    try { setOptions(await onSearch(project.extensionId, query)); } catch (reason) { setError(collaborationErrorMessage(reason)); }
  };
  const mutate = async (action: () => Promise<unknown>) => {
    setWorking(true); setError('');
    try { await action(); await load(); } catch (reason) { setError(collaborationErrorMessage(reason)); }
    finally { setWorking(false); }
  };
  if (!accountAuthorized) return <div className="development-notice"><Users size={16} /><div><strong>请先登录 HiMind</strong><span>登录后可查看和管理项目成员。</span></div></div>;
  if (loading) return <div className="development-collaboration-loading"><BusyIndicator size={16} />正在读取协作者</div>;
  if (!collaboration && error) return <p className="development-inline-error">{error}</p>;
  if (!collaboration?.registered) return <div className="development-notice"><GitBranch size={16} /><div><strong>尚未启用协作</strong><span>在设置中关联代码仓库后即可邀请贡献者。</span></div></div>;
  const members = collaboration.members.filter(item => item.status !== 'declined');
  return <>
    <section className="development-section development-collaboration-section">
      <div className="development-section-heading"><div><h4>项目成员</h4><span>{members.filter(item => item.status === 'active').length} 人</span></div><Pill kind="neutral">我的角色：{roleLabel(collaboration.role || '')}</Pill></div>
      <div className="development-member-list">{members.map(member => <article key={member.id}>
        <span className="development-member-avatar">{member.user_name.trim().slice(0, 1) || '?'}</span>
        <div><strong>{member.user_name || member.user_id}</strong><small>{member.status === 'pending' ? '待接受邀请' : member.role === 'owner' ? '作者' : '已加入'}</small></div>
        <span className="development-member-role">{roleLabel(member.role)}</span>
        {collaboration.can_manage && member.role !== 'owner' ? <button className="btn btn-icon btn-danger-quiet" title="移除协作者" aria-label={`移除 ${member.user_name}`} disabled={working || Boolean(busyAction)} onClick={() => { void confirm({ title: `移除协作者「${member.user_name}」？`, description: '移除后对方不再出现在协作名单里，需要时可重新邀请。', confirmText: '移除' }).then(accepted => { if (accepted) void mutate(() => onRemove(project.extensionId, member.user_id)); }); }}><Trash2 size={15} /></button> : null}
      </article>)}</div>
    </section>
    {collaboration.can_manage && collaboration.source_repository ? <section className="development-section development-invite-section"><h4>邀请贡献者</h4><div className="development-collaborator-search"><label><Search size={15} /><input value={query} placeholder="搜索姓名或部门" onChange={event => setQuery(event.target.value)} onKeyDown={event => { if (event.key === 'Enter') void search(); }} /></label><button className="btn" disabled={working} onClick={() => void search()}>搜索</button></div><div className="development-invite-controls"><select value={selectedUser} onChange={event => setSelectedUser(event.target.value)}><option value="">选择成员</option>{options.map(item => <option key={item.id} value={item.id}>{item.name}{item.department_names.length ? ` · ${item.department_names.join(' / ')}` : ''}</option>)}</select><button className="btn btn-primary" disabled={!selectedUser || working || Boolean(busyAction)} onClick={() => void mutate(async () => { await onInvite(project.extensionId, selectedUser); setSelectedUser(''); })}><UserPlus size={15} />发送邀请</button></div></section> : null}
    {collaboration.can_manage && !collaboration.source_repository ? <div className="development-notice"><GitBranch size={16} /><div><strong>关联代码仓库后可邀请</strong><span>协作项目必须提供 Git 仓库和仓库内目录。</span></div></div> : null}
    {error ? <p className="development-inline-error">{error}</p> : null}
  </>;
}

function collaborationErrorMessage(reason: unknown) {
  const message = reason instanceof Error ? reason.message : String(reason || '');
  const normalized = message.toLowerCase();
  if (normalized.includes('invalid_grant') || normalized.includes('invalid_token') || normalized.includes('refresh token') || normalized.includes('授权已失效') || normalized.includes('请先登录')) {
    return 'HiMind 账号授权已失效，请重新登录。';
  }
  return message || '暂时无法读取协作者，请稍后重试。';
}

function BuildSummary({ project, dashboardEnabled, draft, active, busy, onSubmit, onPublishInstruction }: { project: ProjectModel; dashboardEnabled: boolean; draft: DraftRef; active?: SubmissionRef; busy: boolean; onSubmit: DevelopmentPageProps['onSubmit']; onPublishInstruction: DevelopmentPageProps['onPublishInstruction'] }) {
  const id = project.extensionId;
  const version = draftVersion(draft);
  // 项目规则不进工作台审核：构建即生成并确认候选，收敛动作是发布到本机规则库。
  if (draft.kind === 'instruction') {
    const published = Boolean(draft.value.published_at);
    return <div className="development-build-summary"><div><span><CheckCircle2 size={16} /></span><div><strong>{published ? '已发布到本机规则库' : draft.value.confirmed_at ? '规则制品 · 待发布' : '规则制品已生成'} · v{version}</strong><small>{formatStamp(draft.value.updated_at)} · SHA {shortSha(draft.value.candidate_sha256)}</small></div></div>{published ? <small>可在「AI 对话 → 项目规则」里选择并同步到客户端。</small> : <button className="btn btn-primary" disabled={busy} onClick={() => void onPublishInstruction(id, version)}><Send size={15} />发布到规则库</button>}</div>;
  }
  if (!dashboardEnabled) {
    return <div className="development-build-summary"><div><span><CheckCircle2 size={16} /></span><div><strong>{draft.value.tested_at ? '测试制品 · 已启用' : '测试制品已生成'} · v{version}</strong><small>{formatStamp(draft.value.updated_at)} · SHA {shortSha(draft.value.candidate_sha256)}</small></div></div><small>{draft.value.tested_at ? '当前制品只在本机生效，可以继续调试或通过来源、安装包分发。' : '测试制品已生成，可以继续测试。'}</small></div>;
  }
  const published = active?.value.version === version && active.value.release_status === 'published';
  const submitted = buildMatchesSubmission(draft, active) || (project.kind === 'workflow' && Boolean(draft.value.submitted_at));
  const canSubmit = projectCanSubmit(project);
  const sourceReady = projectSourceReady(project);
  return <div className="development-build-summary"><div><span><CheckCircle2 size={16} /></span><div><strong>{published ? '组织发布版' : submitted ? '工作台审核中' : '测试制品 · 待提交'} · v{version}</strong><small>{formatStamp(draft.value.updated_at)} · SHA {shortSha(draft.value.candidate_sha256)}</small></div></div>{published ? <small>该制品已发布并可供安装，请更新版本号后继续开发。</small> : submitted ? <Pill kind="warn">已提交，等待审核</Pill> : canSubmit && sourceReady ? <button className="btn btn-primary" disabled={busy} onClick={() => void onSubmit(project.kind, id, version)}><Send size={15} />{active ? '更新提交' : '提交审核'}</button> : <small>{project.kind === 'workflow' ? (draft.kind === 'workflow' && draft.value.lock ? '测试制品已通过检查，等待提交审核' : '工作流还未完成检查') : sourceReady ? '当前账号不能提交审核' : '未能读取代码版本，请确认项目位于 Git 仓库中'}</small>}</div>;
}

function ReleasePanel({ project, dashboardEnabled, draft, active, busy, onSubmit, onPublish }: { project: ProjectModel; dashboardEnabled: boolean; draft?: DraftRef; active?: SubmissionRef; busy: boolean; onSubmit: DevelopmentPageProps['onSubmit']; onPublish: DevelopmentPageProps['onPublishDistribution'] }) {
  // 项目规则没有仓库与工作台分发端点，收敛动作在本机规则库，这里只说明去向。
  if (project.kind === 'instruction') {
    return <div className="development-notice"><FileText size={16} /><div><strong>项目规则在本机发布</strong><span>在本机规则库发布后，可在「AI 对话 → 项目规则」选择并同步到客户端。</span></div></div>;
  }
  const versions = projectVersions(project, dashboardEnabled);
  if (!dashboardEnabled) {
    return <>
      <DistributionTargetSummary project={project} />
      <DistributionReleasePanel project={project} draft={draft} busy={busy} onPublish={onPublish} />
      {draft ? <section className="development-release-callout"><div><strong>测试制品</strong><span>当前制品只在本机生效；对外分发请走来源或安装包。</span></div></section> : <div className="development-notice"><CircleAlert size={16} /><div><strong>尚未构建</strong><span>完成构建后即可生成测试制品。</span></div></div>}
      <section className="development-section"><h4>版本历史</h4><div className="development-version-list">{versions.map(version => <article key={version.version}><div><strong>v{version.version}</strong><Pill kind={version.state.tone}>{version.state.label}</Pill></div><small>{version.updatedAt ? formatStamp(version.updatedAt) : '测试制品'}</small>{version.notes ? <p>{version.notes}</p> : null}</article>)}</div></section>
    </>;
  }
  const currentPublished = Boolean(draft && project.submissions.some(item => item.value.version === draftVersion(draft) && item.value.release_status === 'published'));
  const submittedBuild = buildMatchesSubmission(draft, active);
  const canSubmit = projectCanSubmit(project);
  const sourceReady = projectSourceReady(project);
  const activeDraft = active ? project.drafts.find(item => draftVersion(item) === active.value.version) : undefined;
  return <>
    <DistributionTargetSummary project={project} />
    <DistributionReleasePanel project={project} draft={draft} busy={busy} onPublish={onPublish} />
    {active ? <ActiveReview submission={active} draft={activeDraft} /> : null}
    {draft && !currentPublished && !submittedBuild ? <section className="development-release-callout"><div><strong>{canSubmit && sourceReady ? (active ? '有新的测试制品' : '测试制品已就绪') : '测试制品待处理'}</strong><span>{project.kind === 'workflow' ? '工作流检查已完成，提交后进入工作台审核。' : !sourceReady ? '未能读取代码版本，请确认项目位于 Git 仓库中。' : canSubmit ? (active ? '提交后将替代当前等待审核的制品。' : '提交最近一次测试制品进入工作台审核。') : '当前账号不能提交审核。'}</span></div>{canSubmit && sourceReady ? <button className="btn btn-primary" disabled={busy} onClick={() => void onSubmit(project.kind, project.extensionId, draftVersion(draft))}><Send size={15} />{active ? '更新提交' : '提交审核'}</button> : null}</section> : null}
    {!active && !draft ? <div className="development-notice"><CircleAlert size={16} /><div><strong>尚未构建</strong><span>{dashboardEnabled ? '完成构建后即可提交审核。' : '完成构建后即可测试，并通过来源或安装包分发。'}</span></div></div> : null}
    <section className="development-section"><h4>版本历史</h4><div className="development-version-list">{versions.map(version => <article key={version.version}><div><strong>v{version.version}</strong><Pill kind={version.state.tone}>{version.state.label}</Pill></div><small>{version.updatedAt ? formatStamp(version.updatedAt) : '测试制品'}</small>{version.notes ? <p>{version.notes}</p> : null}</article>)}</div></section>
  </>;
}

/**
 * GitHub 发布面板。
 *
 * 目标里没有 GitHub 时不渲染；有 GitHub 时展示仓库、tag、制品与授权状态，
 * 以及分发台账里这一版的结果（已发布 / 部分完成 / 失败可重试）。
 */
function DistributionReleasePanel({ project, draft, busy, onPublish }: { project: ProjectModel; draft?: DraftRef; busy: boolean; onPublish: DevelopmentPageProps['onPublishDistribution'] }) {
  const local = project.local;
  const version = draft ? draftVersion(draft) : '';
  const targets = local?.distribution_targets || [];
  const includesGithub = targets.includes('github');
  const [entries, setEntries] = useState<DistributionStateEntry[]>([]);
  const [preview, setPreview] = useState<DistributionPreview | null>(null);
  const [readError, setReadError] = useState('');
  const [loading, setLoading] = useState(false);
  useEffect(() => {
    if (!local || !includesGithub) return;
    let disposed = false;
    setLoading(true);
    void (async () => {
      try {
        const items = await agentApi.extensionDistributionState(project.kind, project.extensionId);
        if (disposed) return;
        setEntries(items);
      } catch (error) {
        if (!disposed) setReadError(failureText(error, '读取分发台账失败'));
      }
      if (!version) {
        if (!disposed) setPreview(null);
        return;
      }
      try {
        const plan = await agentApi.previewExtensionDistribution(project.kind, project.extensionId, version);
        if (!disposed) {
          setPreview(plan);
          setReadError('');
        }
      } catch (error) {
        // 候选未测试/未确认时预览会失败，这里给出原因而不是留空。
        if (!disposed) {
          setPreview(null);
          setReadError(failureText(error, '读取发布计划失败'));
        }
      } finally {
        if (!disposed) setLoading(false);
      }
    })();
    return () => { disposed = true; };
  }, [local?.id, project.kind, project.extensionId, version, includesGithub]);
  if (!local || !includesGithub) return null;
  const githubEntry = entries.find(item => item.target === 'github' && item.version === version);
  const workbenchEntry = entries.find(item => item.target === 'workbench' && item.version === version);
  const bothTargets = targets.includes('workbench');
  // 发布计划判定"能不能发"：后端认为有阻断时按钮就不可点，避免用户点了才知道被拒。
  const plan = preview?.plan;
  const canPublish = Boolean(draft) && Boolean(version) && !busy && (plan?.ready ?? true);
  return <section className="development-section">
    <h4>发布到 GitHub</h4>
    <dl className="development-settings-list">
      <div><dt>仓库</dt><dd><code>{preview?.github.repository || local.source_repository || '未绑定'}</code></dd></div>
      <div><dt>Tag</dt><dd><code>{preview?.github.tag || '—'}</code></dd></div>
      <div><dt>制品</dt><dd>{preview ? `${preview.github.asset_name} · ${formatBytes(preview.github.size_bytes)}` : '—'}</dd></div>
      <div><dt>授权</dt><dd>{preview?.github.authorized
        ? `已授权 · ${preview.github.login}`
        : <button type="button" className="inline-link" onClick={() => void agentApi.openSettingsWindow('settings', 'accounts')}>未授权 · 去设置里绑定 GitHub</button>}</dd></div>
      <div><dt>制品签名</dt><dd>{signatureLabel(preview)}</dd></div>
      {/* 计划面接管依赖说明后就不再重复一行摘要；计划缺失时退回原来的单行结论。 */}
      {plan ? null : <div><dt>依赖锁定</dt><dd>{dependencyPinLabel(preview)}</dd></div>}
    </dl>
    {plan ? <OperationPlanCard plan={plan} heading="发布计划" /> : null}
    {githubEntry ? <small className="development-target-note">{distributionStateLabel(githubEntry)}</small> : null}
    {bothTargets && workbenchEntry ? <small className="development-target-note">{distributionStateLabel(workbenchEntry)}</small> : null}
    {loading && !preview ? <small className="development-target-note">正在读取发布计划…</small> : null}
    {readError ? <small className="development-target-note danger">{readError}</small> : null}
    <div className="development-settings-actions">
      <button className="btn btn-primary" disabled={!canPublish} onClick={() => void onPublish(project.kind, project.extensionId, version)}>
        {githubEntry?.status === 'published' ? '重新发布' : bothTargets ? '发布到 GitHub 与工作台' : '发布到 GitHub'}
      </button>
    </div>
  </section>;
}

// 发布页顶部的分发目标摘要：让「这个版本会发到哪里」在发布前就可见。
function DistributionTargetSummary({ project }: { project: ProjectModel }) {
  if (!project.local) return null;
  return <section className="development-section development-target-summary">
    <h4>分发目标</h4>
    <p><strong>{distributionTargetLabel(project.local.distribution_targets)}</strong><span>{distributionTargetSourceLabel(project.local)}</span></p>
  </section>;
}

function ActiveReview({ submission, draft }: { submission: SubmissionRef; draft?: DraftRef }) {
  const state = submissionStatus(submission);
  const note = submission.value.review_note;
  return <section className={`development-review ${state.tone}`}>
    <div className="development-review-head"><div><span className="development-review-icon">{state.tone === 'success' ? <CheckCircle2 size={18} /> : state.tone === 'danger' ? <CircleAlert size={18} /> : <Clock3 size={18} />}</span><span><strong>{state.label}</strong><small>v{submission.value.version}</small></span></div><time>{formatStamp(submission.value.updated_at)}</time></div>
    <div className="development-review-timeline"><div className="complete"><span /><div><strong>已提交</strong><small>{draftSubmittedAt(draft) ? formatStamp(draftSubmittedAt(draft)!) : '构建已上传'}</small></div></div><div className={state.label === '待审核' ? 'current' : 'complete'}><span /><div><strong>{state.label}</strong><small>最后更新 {formatStamp(submission.value.updated_at)}</small></div></div></div>
    {note ? <div className="development-review-note"><strong>审核意见</strong><p>{note}</p></div> : null}
  </section>;
}

function SettingsPanel({ project, dashboardEnabled, busy, onOpenFolder, onRequestRemove, onUpdateSource, onSetDistributionTargets }: { project: ProjectModel; dashboardEnabled: boolean; busy: boolean; onOpenFolder: (path: string) => void; onRequestRemove: (project: ExtensionProject) => void; onUpdateSource: DevelopmentPageProps['onUpdateSource']; onSetDistributionTargets: DevelopmentPageProps['onSetDistributionTargets'] }) {
  const [source, setSource] = useState<ExtensionProjectSourceInput>(() => projectSource(project));
  const canManageRepository = !project.remote || project.remote.can_manage;
  const localProject = project.local;
  const targetKey = distributionTargetKey(localProject?.distribution_targets);
  // 清单声明是上限：只在声明范围内提供选项，避免选出必然被裁掉的组合。
  const declaredTargets = localProject?.distribution_targets_declared || [];
  const targetOptions = DISTRIBUTION_TARGET_OPTIONS.filter(option => declaredTargets.length === 0 || option.targets.every(target => declaredTargets.includes(target)));
  const change = <K extends keyof ExtensionProjectSourceInput>(key: K, value: ExtensionProjectSourceInput[K]) => setSource(current => ({ ...current, [key]: value }));
  const canSave = Boolean(project.local && source.source_repository.trim() && source.source_default_branch.trim() && source.source_subdirectory.trim());
  return <>
      <section className="development-section"><h4>项目信息</h4><dl className="development-settings-list"><div><dt>类型</dt><dd>{extensionKindLabels[project.kind]}</dd></div><div><dt>扩展 ID</dt><dd><code>{project.extensionId}</code></dd></div><div><dt>来源仓库</dt><dd>{projectSourceLabel(project)}</dd></div><div><dt>项目目录</dt><dd><code>{project.local?.workspace_path || '未关联'}</code></dd></div></dl>{project.local ? <div className="development-settings-actions"><button className="btn" disabled={!project.local.workspace_available} onClick={() => onOpenFolder(project.local!.workspace_path)}><FolderOpen size={15} />打开目录</button><button className="btn btn-danger-quiet" onClick={() => onRequestRemove(project.local!)}><Trash2 size={15} />移除项目</button></div> : null}</section>
    {localProject ? <section className="development-section"><h4>分发目标</h4>
                <p>决定版本发到哪里：工作台面向组织内审核安装，GitHub 以 Release 对外分发；只影响后续版本。</p>
      <div className="development-target-row">
        <div className="segmented-control development-kind-control">
          {targetOptions.map(option => <button key={option.key} type="button" className={targetKey === option.key ? 'active' : ''} disabled={busy} onClick={() => void onSetDistributionTargets(project.kind, project.extensionId, option.targets)}>{option.label}</button>)}
        </div>
        {localProject.distribution_targets_source === 'project' ? <button className="btn" disabled={busy} onClick={() => void onSetDistributionTargets(project.kind, project.extensionId, null)}>恢复继承</button> : null}
      </div>
      {declaredTargets.length && targetOptions.length < DISTRIBUTION_TARGET_OPTIONS.length ? <small className="development-target-note">清单声明：{distributionTargetLabel(declaredTargets)}（在 {manifestFileName(project.kind)} 的 distribution_targets 中修改）</small> : null}
      <small className="development-target-note">{distributionTargetSourceLabel(localProject)}</small>
    </section> : null}
    <section className="development-section"><h4>代码仓库</h4><div className="development-source-form">
      <label className="wide"><span>仓库地址</span><input value={source.source_repository} disabled={!project.local || !canManageRepository} placeholder="https://git.example.com/team/extensions.git" onChange={event => change('source_repository', event.target.value)} /></label>
      <label><span>默认分支</span><input value={source.source_default_branch} disabled={!project.local || !canManageRepository} placeholder="main" onChange={event => change('source_default_branch', event.target.value)} /></label>
      <label><span>仓库内目录</span><input value={source.source_subdirectory} disabled={!project.local || !canManageRepository} placeholder={project.kind === 'plugin' ? 'plugins/my-plugin' : project.kind === 'workflow' ? 'workflows/my-workflow' : project.kind === 'expert' ? 'experts/my-expert' : 'skills/my-skill'} onChange={event => change('source_subdirectory', event.target.value)} /></label>
    </div>{project.local ? <div className="development-settings-actions"><button className="btn btn-primary" disabled={!canSave || busy} onClick={() => void onUpdateSource(project.local!.id, source, dashboardEnabled && canManageRepository)}><Save size={15} />保存</button></div> : null}</section>
  </>;
}

function CreateProjectDialog({ busy, workspaces, defaultWorkspace, onClose, onCreate }: {
  busy: boolean;
  workspaces: { root: string; label: string }[];
  defaultWorkspace: string;
  onClose: () => void;
  onCreate: (input: CreateExtensionProjectInput, parentDir: string) => Promise<ExtensionProject>;
}) {
  const [input, setInput] = useState<CreateExtensionProjectInput>({ kind: 'skill', slug: '', extension_id: '', name: '', description: '', category: 'software-engineering', template: 'readonly-tool' });
  // 项目落在哪个仓库，和它是插件还是技能一样属于创建时的必要决定，不该等建完再挪。
  const [parentDir, setParentDir] = useState(defaultWorkspace.trim());
  const SkillIcon = extensionKindIcons.skill;
  const PluginIcon = extensionKindIcons.plugin;
  const WorkflowIcon = extensionKindIcons.workflow;
  const InstructionIcon = extensionKindIcons.instruction;
  const valid = input.name.trim() && input.slug.trim() && input.description.trim() && input.category && parentDir.trim();
  const change = <K extends keyof CreateExtensionProjectInput>(key: K, value: CreateExtensionProjectInput[K]) => setInput(current => ({ ...current, [key]: value }));
  return <div className="skill-dialog-backdrop"><div className="skill-dialog development-create-dialog" role="dialog" aria-modal="true"><div className="skill-dialog-head"><strong>新建扩展项目</strong><button className="btn btn-icon" aria-label="关闭" onClick={onClose}><X size={16} /></button></div>
    <div className="development-create-form">
      <div className="segmented-control development-kind-control">
        <button type="button" className={input.kind === 'skill' ? 'active' : ''} onClick={() => setInput(current => ({ ...current, kind: 'skill', template: undefined }))}><SkillIcon size={14} />技能</button>
        <button type="button" className={input.kind === 'plugin' ? 'active' : ''} onClick={() => setInput(current => ({ ...current, kind: 'plugin', template: 'readonly-tool' }))}><PluginIcon size={14} />插件</button>
        <button type="button" className={input.kind === 'workflow' ? 'active' : ''} onClick={() => setInput(current => ({ ...current, kind: 'workflow', template: 'strict' }))}><WorkflowIcon size={14} />工作流</button>
        <button type="button" className={input.kind === 'expert' ? 'active' : ''} onClick={() => setInput(current => ({ ...current, kind: 'expert', template: undefined }))}><Blocks size={14} />专家</button>
        <button type="button" className={input.kind === 'instruction' ? 'active' : ''} onClick={() => setInput(current => ({ ...current, kind: 'instruction', template: undefined }))}><InstructionIcon size={14} />项目规则</button>
      </div>
      <label className="wide"><span>建到工作区</span><select value={parentDir} disabled={!workspaces.length} onChange={event => setParentDir(event.target.value)}>
        {workspaces.length ? null : <option value="">未添加工作区</option>}
        {workspaces.map(item => <option key={item.root} value={item.root}>{item.label} · {item.root}</option>)}
      </select></label>
      <label><span>名称</span><input autoFocus value={input.name} onChange={event => change('name', event.target.value)} /></label>
      <label><span>项目标识</span><input value={input.slug} placeholder="commit-summary" onChange={event => change('slug', event.target.value.toLowerCase().replace(/[^a-z0-9-]/g, '-'))} /></label>
      <label className="wide"><span>功能说明</span><textarea rows={3} value={input.description} onChange={event => change('description', event.target.value)} /></label>
      <label><span>功能分类</span><select value={input.category} onChange={event => change('category', event.target.value)}>{FUNCTIONAL_CATEGORIES.map(category => <option key={category.id} value={category.id}>{category.label}</option>)}</select></label>
      {input.kind === 'plugin' ? <label><span>项目模板</span><select value={input.template} onChange={event => change('template', event.target.value as CreateExtensionProjectInput['template'])}><option value="readonly-tool">AI 工具</option><option value="job-worker">后台任务</option><option value="ui-tool">桌面工具</option></select></label> : null}
      {input.kind === 'workflow' ? <label><span>流程模板</span><select value={input.template} onChange={event => change('template', event.target.value as CreateExtensionProjectInput['template'])}><option value="strict">固定流程</option><option value="segmented">分阶段流程</option><option value="flexible">灵活入口出口</option><option value="development-loop">开发循环</option><option value="capability-pipeline">能力流水线</option></select></label> : null}
    </div>
    <div className="skill-dialog-actions"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-primary" disabled={!valid || busy} onClick={() => { void onCreate(input, parentDir.trim()).catch(() => undefined); }}><Plus size={15} />创建项目</button></div>
  </div></div>;
}

function ConfirmRemoveDialog({ project, dashboardEnabled, busy, onClose, onConfirm }: { project: ExtensionProject; dashboardEnabled: boolean; busy: boolean; onClose: () => void; onConfirm: () => Promise<void> }) {
  return <div className="skill-dialog-backdrop"><div className="skill-dialog" role="dialog" aria-modal="true"><div className="skill-dialog-head"><strong>移除项目</strong><button className="btn btn-icon" aria-label="关闭" onClick={onClose}><X size={16} /></button></div><div className="development-remove-copy"><p>“{project.name}”将从扩展开发列表中移除。</p><span>{dashboardEnabled ? '不会删除本地源码、构建结果或已提交的审核。' : '不会删除本地源码或已生成的测试制品。'}</span></div><div className="skill-dialog-actions"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-danger" disabled={busy} onClick={() => void onConfirm()}><Trash2 size={15} />移除项目</button></div></div></div>;
}

function buildProjectModels(props: Pick<DevelopmentPageProps, 'projects' | 'remoteProjects' | 'pluginDrafts' | 'skillDrafts' | 'workflowDrafts' | 'expertDrafts' | 'instructionDrafts' | 'pluginSubmissions' | 'skillSubmissions' | 'workflowSubmissions'>): ProjectModel[] {
  const map = new Map<string, ProjectModel>();
  const ensure = (kind: ExtensionProjectKind, id: string, name = id, description = '') => {
    const key = `${kind}:${id}`;
    let project = map.get(key);
    if (!project) { project = { key, kind, extensionId: id, name, description, drafts: [], submissions: [] }; map.set(key, project); }
    if (name && project.name === project.extensionId) project.name = name;
    if (description && !project.description) project.description = description;
    return project;
  };
  props.remoteProjects.forEach(remote => { const kind = remote.product_type === 'agent_plugin' ? 'plugin' : remote.product_type === 'workflow_package' ? 'workflow' : remote.product_type === 'agent_expert' ? 'expert' : 'skill'; const project = ensure(kind, remote.product_key, remote.name, remote.description); project.remote = remote; project.name = remote.name; project.description = remote.description; });
  props.projects.forEach(local => { const project = ensure(local.kind, local.extension_id, local.name, local.description); project.local = local; project.name = local.name; project.description = local.description; });
  props.pluginDrafts.forEach(value => ensure('plugin', value.manifest.id, value.manifest.name, value.manifest.description).drafts.push({ kind: 'plugin', value }));
  props.skillDrafts.forEach(value => ensure('skill', value.manifest.id, value.manifest.name, value.manifest.description).drafts.push({ kind: 'skill', value }));
  props.workflowDrafts.forEach(value => ensure('workflow', value.manifest.id, value.manifest.name, value.manifest.description).drafts.push({ kind: 'workflow', value }));
  props.expertDrafts.forEach(value => ensure('expert', value.definition.id, value.definition.name, value.definition.description).drafts.push({ kind: 'expert', value }));
  props.instructionDrafts.forEach(value => ensure('instruction', value.manifest.id, value.manifest.name, value.manifest.description).drafts.push({ kind: 'instruction', value }));
  props.pluginSubmissions.forEach(value => ensure('plugin', value.product_key, value.name).submissions.push({ kind: 'plugin', value }));
  props.skillSubmissions.forEach(value => ensure('skill', value.product_key, value.name || value.product_key).submissions.push({ kind: 'skill', value }));
  props.workflowSubmissions.forEach(value => ensure('workflow', value.manifest.id, value.manifest.name).submissions.push({ kind: 'workflow', value }));
  for (const project of map.values()) {
    project.drafts.sort((left, right) => compareVersions(draftVersion(right), draftVersion(left)) || right.value.updated_at.localeCompare(left.value.updated_at));
    project.submissions.sort((left, right) => right.value.updated_at.localeCompare(left.value.updated_at));
  }
  // 只剩草稿或提交记录的项目，说明本机（或工作台）已经没有这个扩展：要么被改名，
  // 要么退出了扩展源清单。它们既没有源码也没有可发布对象，列出来只会变成一串
  // 「未关联」幽灵条目，因此不进入项目列表。
  return [...map.values()]
    .filter(project => Boolean(project.local || project.remote))
    .sort((left, right) => projectUpdatedAt(right).localeCompare(projectUpdatedAt(left)) || left.name.localeCompare(right.name, 'zh-CN'));
}

function draftVersion(draft: DraftRef) { return draft.kind === 'expert' ? draft.value.definition.version : draft.value.manifest.version; }
function draftName(draft: DraftRef) { return draft.kind === 'expert' ? draft.value.definition.name : draft.value.manifest.name; }
function draftDescription(draft: DraftRef) { return draft.kind === 'expert' ? draft.value.definition.description : draft.value.manifest.description; }
function draftReleaseNotes(draft: DraftRef) { return draft.kind === 'expert' ? draft.value.definition.release_notes || '' : draft.value.manifest.release_notes || ''; }
function draftSubmittedId(draft: DraftRef) { return draft.kind === 'plugin' ? draft.value.dashboard_submission_id : draft.kind === 'expert' ? draft.value.dashboard_release_id : draft.kind === 'instruction' ? '' : draft.value.dashboard_draft_id; }
function currentDraft(project: ProjectModel) { return project.drafts.find(item => draftVersion(item) === project.local?.version) || project.drafts[0]; }
/** 工作台提交时间；项目规则的草稿只在本机发布，没有这个字段。 */
function draftSubmittedAt(draft: DraftRef | undefined) {
  return draft && draft.kind !== 'instruction' ? draft.value.submitted_at : undefined;
}
function activeSubmission(project: ProjectModel) {
  const draft = currentDraft(project);
  if (draftSubmittedAt(draft)) {
    const submissionId = draftSubmittedId(draft);
    const exact = project.submissions.find(item => item.value.id === submissionId) || project.submissions.find(item => item.value.version === draftVersion(draft));
    if (exact) return exact;
  }
  return project.submissions.find(item => item.value.status !== 'superseded' && item.value.release_status !== 'published' && item.value.release_status !== 'revoked') || project.submissions.find(item => item.value.release_status === 'published') || project.submissions[0];
}
function projectSubmissionRole(project: ProjectModel) { return project.remote?.role || project.submissions.find(item => item.value.role)?.value.role || ''; }
function projectCanSubmit(project: ProjectModel) { return project.remote?.can_submit ?? ['owner', 'contributor', ''].includes(projectSubmissionRole(project)); }
function projectSource(project: ProjectModel): ExtensionProjectSourceInput {
  return {
    source_repository: project.local?.source_repository || project.remote?.source_repository || '',
    source_default_branch: project.local?.source_default_branch || project.remote?.source_default_branch || 'main',
    source_subdirectory: project.local?.source_subdirectory || project.remote?.source_subdirectory || '.',
    source_commit: project.local?.source_commit || '',
  };
}
function projectSourceReady(project: ProjectModel) {
  const source = projectSource(project);
  return !source.source_repository || Boolean(source.source_commit.trim());
}
function projectUpdatedAt(project: ProjectModel) { const values = [project.local?.updated_at || '', project.remote?.updated_at || '', ...project.drafts.map(item => item.value.updated_at), ...project.submissions.map(item => item.value.updated_at)].sort(); return values[values.length - 1] || ''; }

function projectState(project: ProjectModel, dashboardEnabled: boolean, draft?: DraftRef, submission?: SubmissionRef): { label: string; tone: 'success' | 'warn' | 'danger' | 'neutral' } {
  if (!project.local) return { label: '未关联', tone: 'warn' };
  if (!project.local.workspace_available) return { label: '目录不可用', tone: 'danger' };
  // 有本机目录但当前版本还没构建过：说「开发中」等于什么都没说，
  // 「未构建」既准确又指出了下一步动作。
  if (!draft || draftVersion(draft) !== project.local.version) return { label: '未构建', tone: 'neutral' };
  if (!dashboardEnabled) return { label: '已构建', tone: 'success' };
  if (draft) {
    const currentRelease = project.submissions.find(item => item.value.version === draftVersion(draft) && item.value.release_status === 'published');
    if (currentRelease) return submissionStatus(currentRelease);
  }
  if (buildMatchesSubmission(draft, submission)) return submission ? submissionStatus(submission) : { label: '正在同步审核状态', tone: 'warn' };
  return { label: '可提交', tone: 'success' };
}

function submissionStatus(submission: SubmissionRef): { label: string; tone: 'success' | 'warn' | 'danger' | 'neutral' } {
  if (submission.value.status === 'superseded') return { label: '已被替代', tone: 'neutral' };
  if (submission.value.release_status === 'revoked') return { label: '已撤回', tone: 'danger' };
  if (submission.value.status === 'approved' && submission.value.release_status === 'published') return { label: '已发布', tone: 'success' };
  if (submission.value.status === 'approved') return { label: '待发布', tone: 'success' };
  if (submission.value.status === 'changes_requested') return { label: '需要修改', tone: 'warn' };
  if (submission.value.status === 'rejected') return { label: '未通过', tone: 'danger' };
  return { label: '待审核', tone: 'warn' };
}

function draftDependencies(draft: DraftRef | undefined, available: PluginCatalogItem[]) {
  if (!draft || draft.kind === 'expert' || draft.kind === 'instruction') return [];
  const names = new Map(available.map(item => [item.plugin_id, item.name]));
  return (draft.value.manifest.plugin_dependencies || []).map(item => ({ id: item.plugin_id, name: names.get(item.plugin_id) || item.plugin_id, required: item.required, version: item.min_version ? `v${item.min_version} 及以上` : '' }));
}

function projectVersions(project: ProjectModel, dashboardEnabled: boolean) {
  const versions = new Map<string, { version: string; notes: string; updatedAt: string; state: { label: string; tone: 'success' | 'warn' | 'danger' | 'neutral' } }>();
  for (const draft of project.drafts) versions.set(draftVersion(draft), { version: draftVersion(draft), notes: draftReleaseNotes(draft), updatedAt: draft.value.updated_at, state: { label: '已构建', tone: 'neutral' } });
  if (!dashboardEnabled) return [...versions.values()].sort((left, right) => compareVersions(right.version, left.version));
  for (const submission of [...project.submissions].reverse()) { const existing = versions.get(submission.value.version); versions.set(submission.value.version, { version: submission.value.version, notes: submission.value.release_notes || existing?.notes || '', updatedAt: submission.value.updated_at, state: submissionStatus(submission) }); }
  const draft = currentDraft(project);
  const submission = activeSubmission(project);
  if (draft && !buildMatchesSubmission(draft, submission) && !project.submissions.some(item => item.value.version === draftVersion(draft) && item.value.release_status === 'published')) {
    const existing = versions.get(draftVersion(draft));
    versions.set(draftVersion(draft), { version: draftVersion(draft), notes: draftReleaseNotes(draft) || existing?.notes || '', updatedAt: draft.value.updated_at, state: { label: '可提交', tone: 'success' } });
  }
  return [...versions.values()].sort((left, right) => compareVersions(right.version, left.version));
}

function buildMatchesSubmission(draft?: DraftRef, submission?: SubmissionRef) {
  if (!draft || !submission || draftVersion(draft) !== submission.value.version) return false;
  const sha = submission.value.sha256?.trim();
  if (sha) return sha.toLowerCase() === draft.value.candidate_sha256.toLowerCase();
  const submissionId = draftSubmittedId(draft);
  return Boolean(draftSubmittedAt(draft) && submissionId === submission.value.id);
}

function shortSha(value: string) { return value ? value.slice(0, 8) : '--'; }

/** 分发目标的三档表达：仅工作台 / 仅 GitHub / 两者。 */
const DISTRIBUTION_TARGET_OPTIONS: { key: 'workbench' | 'github' | 'both'; label: string; targets: DistributionTarget[] }[] = [
  { key: 'workbench', label: '仅工作台', targets: ['workbench'] },
  { key: 'github', label: '仅 GitHub', targets: ['github'] },
  { key: 'both', label: '两者', targets: ['workbench', 'github'] },
];

function distributionTargetKey(targets?: DistributionTarget[]) {
  const normalized = [...(targets || [])].sort();
  if (normalized.length > 1) return 'both';
  return normalized[0] === 'github' ? 'github' : 'workbench';
}

function distributionTargetLabel(targets?: DistributionTarget[]) {
  const key = distributionTargetKey(targets);
  return DISTRIBUTION_TARGET_OPTIONS.find(option => option.key === key)?.label || '仅工作台';
}

function distributionTargetSourceLabel(project: ExtensionProject) {
  const declared = project.distribution_targets_declared || [];
  switch (project.distribution_targets_source) {
    case 'manifest': return `由清单声明的 ${distributionTargetLabel(declared)} 限定，本机设置只能在其范围内收窄。`;
    case 'project': return '已为本项目单独设置，可恢复继承分发单元默认。';
    case 'unit': return '继承自分发单元默认设置。';
    default: return declared.length ? `由清单声明的 ${distributionTargetLabel(declared)} 决定。` : '使用默认值：仅发布到工作台。';
  }
}

/** 分发落点写在哪份清单里：与扩展开发四件套的目录约定保持一致。 */
function manifestFileName(kind: ExtensionProjectKind) {
  return kind === 'plugin' ? 'plugin.json' : kind === 'workflow' ? 'workflow.json' : kind === 'expert' ? 'expert.json' : kind === 'instruction' ? 'instruction.json' : 'skill.json';
}

/** 依赖锁定情况：让「这次发布能否被消费侧精确还原依赖」在发布前就可见。 */
function dependencyPinLabel(preview: DistributionPreview | null) {
  if (!preview) return '—';
  const summary = preview.github.dependencies;
  if (!summary || !summary.total) return '无依赖';
  if (summary.blocked) return `必需依赖无法定位（${summary.unpinned.join('、') || '未知'}）`;
  if (summary.pinned === summary.total) return `${summary.total} 项已锁定版本与摘要`;
  return `${summary.total} 项中 ${summary.pinned} 项已锁定，未锁定：${summary.unpinned.join('、')}`;
}

/** 签名状态：配了私钥就带签名发布；没配说明按未签名发布，需要签名的目录不会收录。 */
function signatureLabel(preview: DistributionPreview | null) {
  if (!preview) return '—';
  const signature = preview.github.signature;
  if (signature?.error) return signature.error;
  if (signature?.configured) return `已配置私钥 · ${signature.key_id}`;
  return '未配置私钥，按未签名发布';
}

/** 台账状态文案：把「发到哪、发到哪一步」讲清楚。 */
function distributionStateLabel(entry: DistributionStateEntry) {
  const target = entry.target === 'github' ? 'GitHub' : '工作台';
  const when = entry.published_at ? ` · ${formatStamp(entry.published_at)}` : '';
  if (entry.status === 'published') {
    return entry.target === 'github'
      ? `${target}已发布${when}${entry.tag ? ` · ${entry.tag}` : ''}`
      : `${target}已提交${when}${entry.submission_id ? ` · ${entry.submission_id}` : ''}`;
  }
  if (entry.status === 'failed') return `${target}发布失败${when} · ${entry.error || '未知错误'}`;
  return `${target}发布中${when}`;
}

function formatBytes(value: number) {
  if (!value) return '0 B';
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / 1024 / 1024).toFixed(1)} MB`;
}
/**
 * 项目归属的扩展源短名。分发单元键形如 `remote:owner/repo`，来源仓库字段形如
 * `owner/repo`；两者都取最后一段，未绑定来源时明确说「未关联来源」。
 */
function projectSourceIdentity(project: ProjectModel) {
  const raw = (project.local?.source_unit_key || project.local?.source_repository
    || project.remote?.source_repository || '').trim();
  if (!raw) return '';
  const withoutKind = /^(?:local|remote):/i.test(raw) ? raw.slice(raw.indexOf(':') + 1) : raw;
  return withoutKind.split('#')[0] || withoutKind;
}

function projectSourceLabel(project: ProjectModel) {
  const identity = projectSourceIdentity(project);
  if (!identity) return '未关联来源';
  const segments = identity.split(/[\\/]/).filter(Boolean);
  return segments[segments.length - 1] || identity;
}

/// 项目落在哪个工作区目录：列表按它分组，同一目录的项目才有"一起开发"的意义。
function projectWorkspaceRoot(project: ProjectModel) {
  return (project.local?.workspace_path || '').trim();
}

/// 项目归属哪个工作区：取「目录包含这个扩展」的工作区根目录（最长匹配优先，
/// 因为工作区可以互相嵌套）。返回空字符串表示它还没有本地目录（工作台项目）。
function workspaceKeyFor(project: ProjectModel, items: { key: string }[]) {
  const own = normalizeFsPath(projectWorkspaceRoot(project));
  if (!own) return '';
  let best = '';
  for (const item of items) {
    if (own !== item.key && !own.startsWith(`${item.key}/`)) continue;
    if (item.key.length > best.length) best = item.key;
  }
  return best || own;
}

/// 工作区展示名：优先用来源名，退回目录名，避免整条绝对路径占满一行。
function workspaceLabelFromRoot(root: string) {
  const segments = root.replace(/[\\/]+$/, '').split(/[\\/]/).filter(Boolean);
  return segments[segments.length - 1] || root || '未记录工作区';
}

/// 工作区下拉显示本地源的名字，兜底用目录名，避免暴露仓库全路径占满一行。
function sourceName(source: ExtensionSourceConfig) {
  const name = (source.name || '').trim();
  if (name && !looksLikeLocalPath(name) && normalizeFsPath(name) !== normalizeFsPath(source.repository)) return name;
  const segments = source.repository.replace(/[\\/]+$/, '').split(/[\\/]/).filter(Boolean);
  return segments[segments.length - 1] || source.repository;
}

function looksLikeLocalPath(value: string) {
  return /^[a-z]:[\\/]/i.test(value) || value.includes('\\\\');
}

function normalizeFsPath(value: string) {
  return value.trim().replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();
}

/// 列表副行只保留「类型 + 版本」：类型已由左侧图标表达，完整信息在悬停提示里。
/**
 * 列表第二行只说「哪个类型、哪个版本」。来源已经由顶部工作区下拉和详情页交代，
 * 在这里再拼一次会把自己挤成省略号，反而看不出最关键的信息。
 */
function listMeta(project: ProjectModel) {
  if (!project.local) return project.remote ? `${extensionKindLabels[project.kind]} · 未关联本地项目` : `${extensionKindLabels[project.kind]} · 未关联来源`;
  return `${extensionKindLabels[project.kind]} · v${project.local.version}`;
}

function extensionKindLabel(productType: string) {
  if (productType === 'agent_plugin') return '插件';
  if (productType === 'workflow_package') return '工作流';
  return '技能';
}
function roleLabel(role: string) { return role === 'owner' ? '作者' : role === 'contributor' ? '贡献者' : '--'; }
