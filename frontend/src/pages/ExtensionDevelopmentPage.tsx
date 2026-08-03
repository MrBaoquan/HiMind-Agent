import { useEffect, useMemo, useState } from 'react';
import { Blocks, BookOpen, CheckCircle2, CircleAlert, Clock3, FolderOpen, GitBranch, Hammer, Inbox, Plus, RefreshCw, Save, Search, Send, Trash2, UserPlus, Users, X } from 'lucide-react';
import { EmptyState, PageHeader, Pill } from '../components/Common';
import { FUNCTIONAL_CATEGORIES } from '../data/categoryCatalog';
import type { AuthoringPluginDraft, AuthoringSkillDraft, CreateExtensionProjectInput, ExtensionCollaboration, ExtensionCollaborationInvitation, ExtensionCollaboratorOption, ExtensionProject, ExtensionProjectKind, ExtensionProjectSourceInput, ExtensionRemoteProject, PluginCatalogItem, PluginSubmissionStatus, SkillSubmissionStatus } from '../services/agentApi';

type DraftRef =
  | { kind: 'plugin'; value: AuthoringPluginDraft }
  | { kind: 'skill'; value: AuthoringSkillDraft };

type SubmissionRef =
  | { kind: 'plugin'; value: PluginSubmissionStatus }
  | { kind: 'skill'; value: SkillSubmissionStatus };

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

type DevelopmentPageProps = {
  projects: ExtensionProject[];
  remoteProjects: ExtensionRemoteProject[];
  pluginDrafts: AuthoringPluginDraft[];
  skillDrafts: AuthoringSkillDraft[];
  pluginSubmissions: PluginSubmissionStatus[];
  skillSubmissions: SkillSubmissionStatus[];
  availablePlugins: PluginCatalogItem[];
  invitations: ExtensionCollaborationInvitation[];
  accountAuthorized: boolean;
  busyAction: string | null;
  onRefresh: () => void;
  onCreate: (input: CreateExtensionProjectInput) => Promise<void>;
  onOpenProject: () => Promise<void>;
  onAssociateProject: (project: ExtensionRemoteProject) => Promise<void>;
  onBuild: (projectId: string) => Promise<void>;
  onSubmit: (kind: ExtensionProjectKind, extensionId: string, version: string) => Promise<void>;
  onOpenFolder: (path: string) => void;
  onRemove: (projectId: string) => Promise<void>;
  onUpdateSource: (projectId: string, input: ExtensionProjectSourceInput, syncRemote: boolean) => Promise<void>;
  onLoadCollaboration: (productKey: string) => Promise<ExtensionCollaboration>;
  onSearchCollaborators: (productKey: string, query: string) => Promise<ExtensionCollaboratorOption[]>;
  onInviteCollaborator: (productKey: string, userId: string) => Promise<unknown>;
  onRemoveCollaborator: (productKey: string, userId: string) => Promise<unknown>;
  onRespondInvitation: (invitationId: string, action: 'accept' | 'decline') => Promise<void>;
};

export function ExtensionDevelopmentPage(props: DevelopmentPageProps) {
  const [query, setQuery] = useState('');
  const [kindFilter, setKindFilter] = useState<'all' | ExtensionProjectKind>('all');
  const [selectedKey, setSelectedKey] = useState('');
  const [createOpen, setCreateOpen] = useState(false);
  const [removeProject, setRemoveProject] = useState<ExtensionProject | null>(null);
  const models = useMemo(() => buildProjectModels(props), [props.projects, props.remoteProjects, props.pluginDrafts, props.skillDrafts, props.pluginSubmissions, props.skillSubmissions]);
  const visible = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    return models.filter(project => {
      if (kindFilter !== 'all' && project.kind !== kindFilter) return false;
      return !normalized || `${project.name} ${project.extensionId} ${project.description}`.toLowerCase().includes(normalized);
    });
  }, [kindFilter, models, query]);
  const selected = visible.find(project => project.key === selectedKey) || visible[0] || null;

  useEffect(() => {
    if (selected && selected.key !== selectedKey) setSelectedKey(selected.key);
  }, [selected, selectedKey]);

  return <div className="development-page">
    <PageHeader title="扩展开发" description="本地项目与发布" actions={<>
      <button className="btn" disabled={Boolean(props.busyAction)} onClick={() => void props.onOpenProject()}><FolderOpen size={16} />打开项目</button>
      <button className="btn btn-primary" disabled={Boolean(props.busyAction)} onClick={() => setCreateOpen(true)}><Plus size={16} />新建项目</button>
      <button className="btn btn-icon" title="刷新项目" aria-label="刷新项目" disabled={Boolean(props.busyAction)} onClick={props.onRefresh}><RefreshCw size={16} /></button>
    </>} />
    {props.invitations.length ? <InvitationInbox invitations={props.invitations} busyAction={props.busyAction} onRespond={props.onRespondInvitation} /> : null}
    <div className="development-toolbar">
      <div className="plugin-tabs" role="tablist" aria-label="项目类型">
        <button className={kindFilter === 'all' ? 'active' : ''} onClick={() => setKindFilter('all')}>全部 <span>{models.length}</span></button>
        <button className={kindFilter === 'skill' ? 'active' : ''} onClick={() => setKindFilter('skill')}>技能 <span>{models.filter(item => item.kind === 'skill').length}</span></button>
        <button className={kindFilter === 'plugin' ? 'active' : ''} onClick={() => setKindFilter('plugin')}>插件 <span>{models.filter(item => item.kind === 'plugin').length}</span></button>
      </div>
      <label className="development-search"><Search size={15} /><input value={query} onChange={event => setQuery(event.target.value)} placeholder="搜索项目" /></label>
    </div>
    <section className="development-workspace">
      <aside className="development-project-list">
        <div className="development-list-heading"><strong>项目</strong><span>{visible.length}</span></div>
        <div className="development-list-body">
          {visible.map(project => <ProjectListItem key={project.key} project={project} selected={project.key === selected?.key} onSelect={setSelectedKey} />)}
          {!visible.length ? <EmptyState icon={Blocks} title="没有项目" text="新建项目或打开已有项目。" /> : null}
        </div>
      </aside>
      <main className="development-project-detail">
        {selected ? <ProjectDetail key={selected.key} project={selected} accountAuthorized={props.accountAuthorized} availablePlugins={props.availablePlugins} busyAction={props.busyAction} onOpenProject={props.onOpenProject} onAssociateProject={props.onAssociateProject} onBuild={props.onBuild} onSubmit={props.onSubmit} onOpenFolder={props.onOpenFolder} onRequestRemove={setRemoveProject} onUpdateSource={props.onUpdateSource} onLoadCollaboration={props.onLoadCollaboration} onSearchCollaborators={props.onSearchCollaborators} onInviteCollaborator={props.onInviteCollaborator} onRemoveCollaborator={props.onRemoveCollaborator} /> : <EmptyState icon={Hammer} title="选择一个项目" text="查看本地工程、构建和发布进度。" />}
      </main>
    </section>
    {createOpen ? <CreateProjectDialog busy={Boolean(props.busyAction)} onClose={() => setCreateOpen(false)} onCreate={async input => { try { await props.onCreate(input); setCreateOpen(false); } catch { /* The parent keeps the dialog open and shows the error. */ } }} /> : null}
    {removeProject ? <ConfirmRemoveDialog project={removeProject} busy={Boolean(props.busyAction)} onClose={() => setRemoveProject(null)} onConfirm={async () => { await props.onRemove(removeProject.id); setRemoveProject(null); }} /> : null}
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
      <div><strong>{item.product_name}</strong><small>{item.product_type === 'agent_plugin' ? '插件' : '技能'} · {roleLabel(item.role)}{item.invited_by_name ? ` · ${item.invited_by_name}` : ''}</small></div>
      <div><button className="btn" disabled={Boolean(busyAction)} onClick={() => void respond(item.id, 'decline')}>拒绝</button><button className="btn btn-primary" disabled={Boolean(busyAction)} onClick={() => void respond(item.id, 'accept')}>接受</button></div>
    </article>)}</div>
    {error ? <p className="development-inline-error">{error}</p> : null}
  </section>;
}

function ProjectListItem({ project, selected, onSelect }: { project: ProjectModel; selected: boolean; onSelect: (key: string) => void }) {
  const draft = currentDraft(project);
  const active = activeSubmission(project);
  const state = projectState(project, draft, active);
  return <button className={`development-project-item ${selected ? 'selected' : ''}`} onClick={() => onSelect(project.key)}>
    <span className={`development-kind-mark ${project.kind}`}>{project.kind === 'plugin' ? 'P' : 'S'}</span>
    <span className="development-project-copy"><strong>{project.name}</strong><small>{kindLabel(project.kind)} · {project.local ? `本地 v${project.local.version}` : '未关联本地项目'}</small>{active ? <small>{submissionStatus(active).label} · v{active.value.version}</small> : null}</span>
    <span className={`skill-state-label ${state.tone}`}>{state.label}</span>
  </button>;
}

function ProjectDetail({ project, accountAuthorized, availablePlugins, busyAction, onOpenProject, onAssociateProject, onBuild, onSubmit, onOpenFolder, onRequestRemove, onUpdateSource, onLoadCollaboration, onSearchCollaborators, onInviteCollaborator, onRemoveCollaborator }: {
  project: ProjectModel;
  accountAuthorized: boolean;
  availablePlugins: PluginCatalogItem[];
  busyAction: string | null;
  onOpenProject: () => Promise<void>;
  onAssociateProject: DevelopmentPageProps['onAssociateProject'];
  onBuild: (projectId: string) => Promise<void>;
  onSubmit: DevelopmentPageProps['onSubmit'];
  onOpenFolder: (path: string) => void;
  onRequestRemove: (project: ExtensionProject) => void;
  onUpdateSource: DevelopmentPageProps['onUpdateSource'];
  onLoadCollaboration: DevelopmentPageProps['onLoadCollaboration'];
  onSearchCollaborators: DevelopmentPageProps['onSearchCollaborators'];
  onInviteCollaborator: DevelopmentPageProps['onInviteCollaborator'];
  onRemoveCollaborator: DevelopmentPageProps['onRemoveCollaborator'];
}) {
  const [tab, setTab] = useState<'overview' | 'release' | 'collaboration' | 'settings'>('overview');
  const draft = currentDraft(project);
  const active = activeSubmission(project);
  const state = projectState(project, draft, active);
  const version = draft?.value.manifest.version || project.local?.version || active?.value.version || '--';
  const busy = Boolean(busyAction);
  const dependencies = draftDependencies(draft, availablePlugins);

  return <>
    <header className="development-detail-header">
      <div className="development-detail-title"><span className={`development-kind-mark ${project.kind}`}>{project.kind === 'plugin' ? 'P' : 'S'}</span><div><div><h3>{project.name}</h3><Pill kind={state.tone}>{state.label}</Pill></div><small>{kindLabel(project.kind)} · v{version}</small></div></div>
      <div className="development-detail-actions">
        {project.local?.workspace_available ? <button className="btn" onClick={() => onOpenFolder(project.local!.workspace_path)}><FolderOpen size={15} />打开目录</button> : <button className="btn" onClick={() => void (project.remote ? onAssociateProject(project.remote) : onOpenProject())}><FolderOpen size={15} />关联项目</button>}
        {project.local?.workspace_available ? <button className="btn btn-primary" disabled={busy} onClick={() => void onBuild(project.local!.id)}><Hammer className={busyAction === `build:${project.local.id}` ? 'spin' : ''} size={15} />构建</button> : null}
      </div>
    </header>
    <div className="extension-detail-tabs development-detail-tabs" role="tablist">
      {([['overview', '概览'], ['release', '发布'], ['collaboration', '协作者'], ['settings', '设置']] as const).map(item => <button key={item[0]} className={tab === item[0] ? 'active' : ''} onClick={() => setTab(item[0])}>{item[1]}{item[0] === 'release' && active && ['changes_requested', 'rejected'].includes(active.value.status) ? <span className="tab-alert" /> : null}</button>)}
    </div>
    <div className="development-detail-body">
      {tab === 'overview' ? <>
        {!project.local?.workspace_available ? <div className="development-notice"><CircleAlert size={16} /><div><strong>未关联本地项目</strong><span>选择包含 plugin.json 或 skill.json 的项目目录。</span></div></div> : null}
        <section className="development-section"><h4>项目说明</h4><p>{project.description || draft?.value.manifest.description || '未提供项目说明。'}</p></section>
        <section className="development-section"><h4>最近构建</h4>{draft ? <BuildSummary project={project} draft={draft} active={active} busy={busy} onSubmit={onSubmit} /> : <div className="development-empty-line"><span>尚未构建</span>{project.local?.workspace_available ? <button className="btn btn-primary" disabled={busy} onClick={() => void onBuild(project.local!.id)}><Hammer size={15} />构建</button> : null}</div>}</section>
        <section className="development-section"><h4>依赖</h4>{dependencies.length ? <div className="development-dependency-list">{dependencies.map(item => <div key={item.id}><strong>{item.name}</strong><small>{item.required ? '必需' : '可选'}{item.version ? ` · ${item.version}` : ''}</small></div>)}</div> : <p className="muted">无依赖</p>}</section>
      </> : null}
      {tab === 'release' ? <ReleasePanel project={project} draft={draft} active={active} busy={busy} onSubmit={onSubmit} /> : null}
      {tab === 'collaboration' ? <CollaborationPanel project={project} accountAuthorized={accountAuthorized} busyAction={busyAction} onLoad={onLoadCollaboration} onSearch={onSearchCollaborators} onInvite={onInviteCollaborator} onRemove={onRemoveCollaborator} /> : null}
      {tab === 'settings' ? <SettingsPanel project={project} busy={busy} onOpenFolder={onOpenFolder} onRequestRemove={onRequestRemove} onUpdateSource={onUpdateSource} /> : null}
    </div>
  </>;
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
  if (loading) return <div className="development-collaboration-loading"><RefreshCw className="spin" size={16} />正在读取协作者</div>;
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
        {collaboration.can_manage && member.role !== 'owner' ? <button className="btn btn-icon btn-danger-quiet" title="移除协作者" aria-label={`移除 ${member.user_name}`} disabled={working || Boolean(busyAction)} onClick={() => { if (window.confirm(`确定移除“${member.user_name}”吗？`)) void mutate(() => onRemove(project.extensionId, member.user_id)); }}><Trash2 size={15} /></button> : null}
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

function BuildSummary({ project, draft, active, busy, onSubmit }: { project: ProjectModel; draft: DraftRef; active?: SubmissionRef; busy: boolean; onSubmit: DevelopmentPageProps['onSubmit'] }) {
  const id = project.extensionId;
  const version = draft.value.manifest.version;
  const published = active?.value.version === version && active.value.release_status === 'published';
  const submitted = buildMatchesSubmission(draft, active);
  const canSubmit = projectCanSubmit(project);
  const sourceReady = projectSourceReady(project);
  return <div className="development-build-summary"><div><span><CheckCircle2 size={16} /></span><div><strong>构建完成 · v{version}</strong><small>{formatTime(draft.value.updated_at)} · {shortSha(draft.value.candidate_sha256)}</small></div></div>{published ? <small>该版本已发布，请更新版本号后继续开发。</small> : submitted ? <Pill kind="warn">审核中</Pill> : canSubmit && sourceReady ? <button className="btn btn-primary" disabled={busy} onClick={() => void onSubmit(project.kind, id, version)}><Send size={15} />{active ? '更新提交' : '提交审核'}</button> : <small>{!sourceReady ? '请先在设置中填写源码版本' : '当前账号不能提交审核'}</small>}</div>;
}

function ReleasePanel({ project, draft, active, busy, onSubmit }: { project: ProjectModel; draft?: DraftRef; active?: SubmissionRef; busy: boolean; onSubmit: DevelopmentPageProps['onSubmit'] }) {
  const versions = projectVersions(project);
  const currentPublished = Boolean(draft && project.submissions.some(item => item.value.version === draft.value.manifest.version && item.value.release_status === 'published'));
  const submittedBuild = buildMatchesSubmission(draft, active);
  const canSubmit = projectCanSubmit(project);
  const sourceReady = projectSourceReady(project);
  const activeDraft = active ? project.drafts.find(item => item.value.manifest.version === active.value.version) : undefined;
  return <>
    {active ? <ActiveReview submission={active} draft={activeDraft} /> : null}
    {draft && !currentPublished && !submittedBuild ? <section className="development-release-callout"><div><strong>{canSubmit && sourceReady ? (active ? '有新的构建' : '可以提交审核') : '有新的本地构建'}</strong><span>{!sourceReady ? '在设置中填写本次构建对应的 commit SHA。' : canSubmit ? (active ? '提交后将替代当前等待审核的构建。' : '提交最近一次构建进入审核。') : '当前账号不能提交审核。'}</span></div>{canSubmit && sourceReady ? <button className="btn btn-primary" disabled={busy} onClick={() => void onSubmit(project.kind, project.extensionId, draft.value.manifest.version)}><Send size={15} />{active ? '更新提交' : '提交审核'}</button> : null}</section> : null}
    {!active && !draft ? <div className="development-notice"><CircleAlert size={16} /><div><strong>尚未构建</strong><span>完成构建后即可提交审核。</span></div></div> : null}
    <section className="development-section"><h4>版本历史</h4><div className="development-version-list">{versions.map(version => <article key={version.version}><div><strong>v{version.version}</strong><Pill kind={version.state.tone}>{version.state.label}</Pill></div><small>{version.updatedAt ? formatTime(version.updatedAt) : '本地版本'}</small><p>{version.notes || '未提供更新说明。'}</p></article>)}</div></section>
  </>;
}

function ActiveReview({ submission, draft }: { submission: SubmissionRef; draft?: DraftRef }) {
  const state = submissionStatus(submission);
  const note = submission.value.review_note;
  return <section className={`development-review ${state.tone}`}>
    <div className="development-review-head"><div><span className="development-review-icon">{state.tone === 'success' ? <CheckCircle2 size={18} /> : state.tone === 'danger' ? <CircleAlert size={18} /> : <Clock3 size={18} />}</span><span><strong>{state.label}</strong><small>v{submission.value.version}</small></span></div><time>{formatTime(submission.value.updated_at)}</time></div>
    <div className="development-review-timeline"><div className="complete"><span /><div><strong>已提交</strong><small>{draft?.value.submitted_at ? formatTime(draft.value.submitted_at) : '构建已上传'}</small></div></div><div className={state.label === '待审核' ? 'current' : 'complete'}><span /><div><strong>{state.label}</strong><small>最后更新 {formatTime(submission.value.updated_at)}</small></div></div></div>
    {note ? <div className="development-review-note"><strong>审核意见</strong><p>{note}</p></div> : null}
  </section>;
}

function SettingsPanel({ project, busy, onOpenFolder, onRequestRemove, onUpdateSource }: { project: ProjectModel; busy: boolean; onOpenFolder: (path: string) => void; onRequestRemove: (project: ExtensionProject) => void; onUpdateSource: DevelopmentPageProps['onUpdateSource'] }) {
  const [source, setSource] = useState<ExtensionProjectSourceInput>(() => projectSource(project));
  const canManageRepository = !project.remote || project.remote.can_manage;
  const change = <K extends keyof ExtensionProjectSourceInput>(key: K, value: ExtensionProjectSourceInput[K]) => setSource(current => ({ ...current, [key]: value }));
  const canSave = Boolean(project.local && source.source_repository.trim() && source.source_default_branch.trim() && source.source_subdirectory.trim());
  return <>
    <section className="development-section"><h4>项目信息</h4><dl className="development-settings-list"><div><dt>类型</dt><dd>{kindLabel(project.kind)}</dd></div><div><dt>扩展 ID</dt><dd><code>{project.extensionId}</code></dd></div><div><dt>项目目录</dt><dd><code>{project.local?.workspace_path || '未关联'}</code></dd></div></dl>{project.local ? <div className="development-settings-actions"><button className="btn" disabled={!project.local.workspace_available} onClick={() => onOpenFolder(project.local!.workspace_path)}><FolderOpen size={15} />打开目录</button><button className="btn btn-danger-quiet" onClick={() => onRequestRemove(project.local!)}><Trash2 size={15} />移出工作台</button></div> : null}</section>
    <section className="development-section"><h4>代码仓库</h4><div className="development-source-form">
      <label className="wide"><span>仓库地址</span><input value={source.source_repository} disabled={!project.local || !canManageRepository} placeholder="https://git.example.com/team/extensions.git" onChange={event => change('source_repository', event.target.value)} /></label>
      <label><span>默认分支</span><input value={source.source_default_branch} disabled={!project.local || !canManageRepository} placeholder="main" onChange={event => change('source_default_branch', event.target.value)} /></label>
      <label><span>仓库内目录</span><input value={source.source_subdirectory} disabled={!project.local || !canManageRepository} placeholder={project.kind === 'plugin' ? 'plugins/my-plugin' : 'skills/my-skill'} onChange={event => change('source_subdirectory', event.target.value)} /></label>
      <label className="wide"><span>源码版本</span><input value={source.source_commit} disabled={!project.local} placeholder="commit SHA" onChange={event => change('source_commit', event.target.value)} /></label>
    </div>{project.local ? <div className="development-settings-actions"><button className="btn btn-primary" disabled={!canSave || busy} onClick={() => void onUpdateSource(project.local!.id, source, canManageRepository)}><Save size={15} />保存</button></div> : null}</section>
  </>;
}

function CreateProjectDialog({ busy, onClose, onCreate }: { busy: boolean; onClose: () => void; onCreate: (input: CreateExtensionProjectInput) => Promise<void> }) {
  const [input, setInput] = useState<CreateExtensionProjectInput>({ kind: 'skill', slug: '', extension_id: '', name: '', description: '', category: 'software-engineering', template: 'readonly-tool' });
  const valid = input.name.trim() && input.slug.trim() && input.description.trim() && input.category;
  const change = <K extends keyof CreateExtensionProjectInput>(key: K, value: CreateExtensionProjectInput[K]) => setInput(current => ({ ...current, [key]: value }));
  return <div className="skill-dialog-backdrop"><div className="skill-dialog development-create-dialog" role="dialog" aria-modal="true"><div className="skill-dialog-head"><strong>新建扩展项目</strong><button className="btn btn-icon" aria-label="关闭" onClick={onClose}><X size={16} /></button></div>
    <div className="development-create-form">
      <div className="segmented-control development-kind-control"><button type="button" className={input.kind === 'skill' ? 'active' : ''} onClick={() => change('kind', 'skill')}><BookOpen size={14} />技能</button><button type="button" className={input.kind === 'plugin' ? 'active' : ''} onClick={() => change('kind', 'plugin')}><Blocks size={14} />插件</button></div>
      <label><span>名称</span><input autoFocus value={input.name} onChange={event => change('name', event.target.value)} /></label>
      <label><span>项目标识</span><input value={input.slug} placeholder="commit-summary" onChange={event => change('slug', event.target.value.toLowerCase().replace(/[^a-z0-9-]/g, '-'))} /></label>
      <label className="wide"><span>功能说明</span><textarea rows={3} value={input.description} onChange={event => change('description', event.target.value)} /></label>
      <label><span>功能分类</span><select value={input.category} onChange={event => change('category', event.target.value)}>{FUNCTIONAL_CATEGORIES.map(category => <option key={category.id} value={category.id}>{category.label}</option>)}</select></label>
      {input.kind === 'plugin' ? <label><span>项目模板</span><select value={input.template} onChange={event => change('template', event.target.value as CreateExtensionProjectInput['template'])}><option value="readonly-tool">AI 工具</option><option value="job-worker">后台任务</option><option value="ui-tool">桌面工具</option></select></label> : null}
    </div>
    <div className="skill-dialog-actions"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-primary" disabled={!valid || busy} onClick={() => void onCreate(input)}><Plus size={15} />创建项目</button></div>
  </div></div>;
}

function ConfirmRemoveDialog({ project, busy, onClose, onConfirm }: { project: ExtensionProject; busy: boolean; onClose: () => void; onConfirm: () => Promise<void> }) {
  return <div className="skill-dialog-backdrop"><div className="skill-dialog" role="dialog" aria-modal="true"><div className="skill-dialog-head"><strong>移出工作台</strong><button className="btn btn-icon" aria-label="关闭" onClick={onClose}><X size={16} /></button></div><div className="development-remove-copy"><p>“{project.name}”将不再显示在扩展开发中。</p><span>本地源码、构建和已提交审核不会被删除。</span></div><div className="skill-dialog-actions"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-danger" disabled={busy} onClick={() => void onConfirm()}><Trash2 size={15} />移出</button></div></div></div>;
}

function buildProjectModels(props: Pick<DevelopmentPageProps, 'projects' | 'remoteProjects' | 'pluginDrafts' | 'skillDrafts' | 'pluginSubmissions' | 'skillSubmissions'>): ProjectModel[] {
  const map = new Map<string, ProjectModel>();
  const ensure = (kind: ExtensionProjectKind, id: string, name = id, description = '') => {
    const key = `${kind}:${id}`;
    let project = map.get(key);
    if (!project) { project = { key, kind, extensionId: id, name, description, drafts: [], submissions: [] }; map.set(key, project); }
    if (name && project.name === project.extensionId) project.name = name;
    if (description && !project.description) project.description = description;
    return project;
  };
  props.remoteProjects.forEach(remote => { const kind = remote.product_type === 'agent_plugin' ? 'plugin' : 'skill'; const project = ensure(kind, remote.product_key, remote.name, remote.description); project.remote = remote; project.name = remote.name; project.description = remote.description; });
  props.projects.forEach(local => { const project = ensure(local.kind, local.extension_id, local.name, local.description); project.local = local; project.name = local.name; project.description = local.description; });
  props.pluginDrafts.forEach(value => ensure('plugin', value.manifest.id, value.manifest.name, value.manifest.description).drafts.push({ kind: 'plugin', value }));
  props.skillDrafts.forEach(value => ensure('skill', value.manifest.id, value.manifest.name, value.manifest.description).drafts.push({ kind: 'skill', value }));
  props.pluginSubmissions.forEach(value => ensure('plugin', value.product_key, value.name).submissions.push({ kind: 'plugin', value }));
  props.skillSubmissions.forEach(value => ensure('skill', value.product_key, value.name || value.product_key).submissions.push({ kind: 'skill', value }));
  for (const project of map.values()) {
    project.drafts.sort((left, right) => compareVersions(right.value.manifest.version, left.value.manifest.version) || right.value.updated_at.localeCompare(left.value.updated_at));
    project.submissions.sort((left, right) => right.value.updated_at.localeCompare(left.value.updated_at));
  }
  return [...map.values()].sort((left, right) => projectUpdatedAt(right).localeCompare(projectUpdatedAt(left)) || left.name.localeCompare(right.name, 'zh-CN'));
}

function currentDraft(project: ProjectModel) { return project.drafts.find(item => item.value.manifest.version === project.local?.version) || project.drafts[0]; }
function activeSubmission(project: ProjectModel) {
  const draft = currentDraft(project);
  if (draft?.value.submitted_at) {
    const submissionId = draft.kind === 'plugin' ? draft.value.dashboard_submission_id : draft.value.dashboard_draft_id;
    const exact = project.submissions.find(item => item.value.id === submissionId) || project.submissions.find(item => item.value.version === draft.value.manifest.version);
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
function projectSourceReady(project: ProjectModel) { const source = projectSource(project); return !source.source_repository || Boolean(source.source_commit.trim()); }
function projectUpdatedAt(project: ProjectModel) { const values = [project.local?.updated_at || '', project.remote?.updated_at || '', ...project.drafts.map(item => item.value.updated_at), ...project.submissions.map(item => item.value.updated_at)].sort(); return values[values.length - 1] || ''; }

function projectState(project: ProjectModel, draft?: DraftRef, submission?: SubmissionRef): { label: string; tone: 'success' | 'warn' | 'danger' | 'neutral' } {
  if (!project.local) return { label: '未关联', tone: 'warn' };
  if (!project.local.workspace_available) return { label: '目录不可用', tone: 'danger' };
  if (draft) {
    const currentRelease = project.submissions.find(item => item.value.version === draft.value.manifest.version && item.value.release_status === 'published');
    if (currentRelease) return submissionStatus(currentRelease);
  }
  if (!draft || draft.value.manifest.version !== project.local.version) return { label: '开发中', tone: 'neutral' };
  if (buildMatchesSubmission(draft, submission)) return submission ? submissionStatus(submission) : { label: '同步审核', tone: 'warn' };
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
  if (!draft) return [];
  const names = new Map(available.map(item => [item.plugin_id, item.name]));
  return (draft.value.manifest.plugin_dependencies || []).map(item => ({ id: item.plugin_id, name: names.get(item.plugin_id) || item.plugin_id, required: item.required, version: item.min_version ? `v${item.min_version} 及以上` : '' }));
}

function projectVersions(project: ProjectModel) {
  const versions = new Map<string, { version: string; notes: string; updatedAt: string; state: { label: string; tone: 'success' | 'warn' | 'danger' | 'neutral' } }>();
  for (const draft of project.drafts) versions.set(draft.value.manifest.version, { version: draft.value.manifest.version, notes: draft.value.manifest.release_notes || '', updatedAt: draft.value.updated_at, state: { label: '已构建', tone: 'neutral' } });
  for (const submission of [...project.submissions].reverse()) { const existing = versions.get(submission.value.version); versions.set(submission.value.version, { version: submission.value.version, notes: submission.value.release_notes || existing?.notes || '', updatedAt: submission.value.updated_at, state: submissionStatus(submission) }); }
  const draft = currentDraft(project);
  const submission = activeSubmission(project);
  if (draft && !buildMatchesSubmission(draft, submission) && !project.submissions.some(item => item.value.version === draft.value.manifest.version && item.value.release_status === 'published')) {
    const existing = versions.get(draft.value.manifest.version);
    versions.set(draft.value.manifest.version, { version: draft.value.manifest.version, notes: draft.value.manifest.release_notes || existing?.notes || '', updatedAt: draft.value.updated_at, state: { label: '可提交', tone: 'success' } });
  }
  return [...versions.values()].sort((left, right) => compareVersions(right.version, left.version));
}

function buildMatchesSubmission(draft?: DraftRef, submission?: SubmissionRef) {
  if (!draft || !submission || draft.value.manifest.version !== submission.value.version) return false;
  const sha = submission.value.sha256?.trim();
  if (sha) return sha.toLowerCase() === draft.value.candidate_sha256.toLowerCase();
  const submissionId = draft.kind === 'plugin' ? draft.value.dashboard_submission_id : draft.value.dashboard_draft_id;
  return Boolean(draft.value.submitted_at && submissionId === submission.value.id);
}

function shortSha(value: string) { return value ? value.slice(0, 8) : '--'; }

function compareVersions(left: string, right: string) { const a = left.split(/[.+-]/).map(value => Number.parseInt(value, 10) || 0); const b = right.split(/[.+-]/).map(value => Number.parseInt(value, 10) || 0); for (let index = 0; index < Math.max(a.length, b.length); index += 1) { const diff = (a[index] || 0) - (b[index] || 0); if (diff) return diff; } return left.localeCompare(right); }
function formatTime(value?: string | null) { if (!value) return '--'; const numeric = Number(value); const date = new Date(Number.isFinite(numeric) && numeric > 0 ? numeric : value); return Number.isNaN(date.getTime()) ? value : date.toLocaleString('zh-CN', { year: 'numeric', month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit' }); }
function kindLabel(kind: ExtensionProjectKind) { return kind === 'plugin' ? '插件' : '技能'; }
function roleLabel(role: string) { return role === 'owner' ? '作者' : role === 'contributor' ? '贡献者' : '--'; }
