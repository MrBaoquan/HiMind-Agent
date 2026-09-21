import { useEffect, useMemo, useState } from 'react';
import {
  BookOpen,
  ArrowLeft,
  BadgeCheck,
  ChevronDown,
  Code2,
  CircleAlert,
  FolderOpen,
  Download,
  Link2Off,
  PlugZap,
  RefreshCw,
  Files,
  Link2,
  MoreHorizontal,
  Search,
  Settings2,
  Sparkles,
  Store,
  Trash2,
  Wrench,
  X,
} from 'lucide-react';
import { EmptyState, PageHeader, Pill, Tags } from '../components/Common';
import type { CodexSkillStatusItem, CodexSkillStatusResponse, ExtensionDesiredItem, ExtensionDesiredState, McpTargetDescriptor, OrganizationSkillCatalogItem, PluginCatalogItem, PluginRegistry, SkillCatalogResponse, SkillInstallPlan, SkillSyncSettings, SkillWorkspaceStatus } from '../services/agentApi';
import { ManagedCapabilitiesPanel } from './ManagedCapabilitiesPage';

type SkillsWorkspacePageProps = {
  catalog: SkillCatalogResponse | null;
  status: CodexSkillStatusResponse | null;
  workspace: SkillWorkspaceStatus;
  mcpTargets: McpTargetDescriptor[];
  error: string | null;
  marketplace: OrganizationSkillCatalogItem[];
	dashboardEnabled: boolean;
	desired: ExtensionDesiredState | null;
	desiredLoading: boolean;
	desiredError: string | null;
	pluginRegistry: PluginRegistry | null;
	catalogEnabled: boolean;
	onOpenExtensions: () => void;
	availablePlugins: PluginCatalogItem[];
  busyAction: string | null;
  onRefresh: () => void;
  onSyncAll: () => void;
  onPickWorkspace: () => void;
  onClearWorkspace: () => void;
  onSyncSkill: (skillId: string) => void;
  onUpdateWorkspace: (skillId: string) => void;
  onSetWorkspaceEnabled: (skillId: string, enabled: boolean) => void;
  syncMode: SkillSyncSettings['mode'];
  onSetSyncMode: (mode: SkillSyncSettings['mode']) => void;
  onLoadVersions: (skillId: string) => Promise<OrganizationSkillCatalogItem[]>;
  onPlanMarketplace: (skillId: string, version?: string) => Promise<SkillInstallPlan>;
  onInstallMarketplace: (skillId: string, version: string | undefined, optionalPluginIds: string[]) => void;
  onRepair: (skillId: string) => void;
  onUninstall: (skillId: string) => void;
  onSyncSkillClient: (skillId: string, clientId: string) => void;
  onUnregisterClient: (skillId: string, clientId: string) => void;
  onUnregisterClients: (skillId: string) => void;
  onOpenDirectory: (path: string) => void;
  onImportLocal: () => void;
  onImportGithub: (sourceUrl: string) => Promise<void>;
  onOpenAiConnections: () => void;
};

type ViewKey = 'installed' | 'system';

export function SkillsWorkspacePage({ catalog, status, workspace, mcpTargets, error, marketplace, desired, desiredLoading, desiredError, pluginRegistry, dashboardEnabled, catalogEnabled, availablePlugins, busyAction, syncMode, onSetSyncMode, onRefresh, onSyncAll, onPickWorkspace, onClearWorkspace, onSyncSkill, onUpdateWorkspace, onSetWorkspaceEnabled, onSyncSkillClient, onLoadVersions, onPlanMarketplace, onInstallMarketplace, onRepair, onUninstall, onUnregisterClient, onUnregisterClients, onOpenDirectory, onImportLocal, onImportGithub, onOpenAiConnections, onOpenExtensions }: SkillsWorkspacePageProps) {
  const [query, setQuery] = useState('');
  const [view, setView] = useState<ViewKey>(() => {
    try {
      const stored = window.localStorage.getItem('himind-agent.skills-view');
      if (stored === 'system' && dashboardEnabled) return 'system';
      return 'installed';
    } catch { return 'installed'; }
  });
  const [selectedId, setSelectedId] = useState('');
  const [detailOpen, setDetailOpen] = useState(false);
  const [pendingUninstall, setPendingUninstall] = useState<CodexSkillStatusItem | null>(null);
  const [installPlan, setInstallPlan] = useState<SkillInstallPlan | null>(null);
  const [planError, setPlanError] = useState('');
  const [githubOpen, setGithubOpen] = useState(false);
  const [githubSourceUrl, setGithubSourceUrl] = useState('');
  const [githubBusy, setGithubBusy] = useState(false);
  const [githubError, setGithubError] = useState('');
  const [planLoading, setPlanLoading] = useState(false);
  const items = useMemo(() => aggregateSkillStatusItems(status), [status]);
	const installedById = useMemo(() => new Map(items.map(item => [item.record.manifest.id, item])), [items]);
	const localItems = useMemo(() => dashboardEnabled ? items.filter(item => !isManagedSkill(item, desired, marketplace)) : items, [dashboardEnabled, desired, items, marketplace]);
	const systemSkillCount = useMemo(() => {
	  const ids = new Set((desired?.items || []).filter(item => item.asset_kind === 'skill' && isManagedPolicy(item)).map(item => item.asset_key));
	  items.filter(item => item.record.manifest.scope === 'builtin').forEach(item => ids.add(item.record.manifest.id));
	  marketplace.filter(item => isManagedCatalogSkill(item)).forEach(item => ids.add(item.skill_id));
	  return ids.size;
	}, [desired, items, marketplace]);
  useEffect(() => {
	    const selectableIds = view === 'installed' ? localItems.map(item => item.record.manifest.id) : [];
    if (!selectableIds.length) {
      setSelectedId('');
    } else if (!selectableIds.includes(selectedId)) {
      setSelectedId(selectableIds[0]);
    }
  }, [view, localItems, selectedId]);

  useEffect(() => { setDetailOpen(false); }, [view]);
  useEffect(() => { try { window.localStorage.setItem('himind-agent.skills-view', view); } catch { /* storage is optional */ } }, [view]);
  useEffect(() => {
    if (!dashboardEnabled && view === 'system') setView('installed');
  }, [dashboardEnabled, view]);

  const filteredItems = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    return localItems.filter(item => {
      if (!normalized) return true;
      const manifest = item.record.manifest;
      return [manifest.id, manifest.name, manifest.description, manifest.risk_summary, ...(manifest.capabilities || []).map(capability => capability.id)].join(' ').toLowerCase().includes(normalized);
    });
  }, [localItems, query]);

  const selected = filteredItems.find(item => item.record.manifest.id === selectedId) || filteredItems[0];
	  const installedCount = localItems.length;
  const isBusy = Boolean(busyAction);
  // A selected project always renders copies; the copy/symlink preference only
  // applies to the global target.
  const effectiveSyncMode: 'copy' | 'symlink' = workspace.valid
    ? (status?.render_mode || 'copy')
    : syncMode;

  async function openInstallPlan(skillId: string, version?: string) {
    setPlanLoading(true);
    setPlanError('');
    try { setInstallPlan(await onPlanMarketplace(skillId, version)); }
    catch { setPlanError('暂时无法检查技能，请稍后重试。'); }
    finally { setPlanLoading(false); }
  }

  if (!catalog && !status && !error) return <div className="page-loading"><span className="spinner" />正在读取技能</div>;

  return (
    <div className="skill-page skill-product-page">
      <PageHeader
        title="技能"
        description="管理已安装技能"
        actions={<>
          <button className="btn" title="浏览技能市场" onClick={onOpenExtensions}><Store size={15} />浏览市场</button>
          <details className="runtime-more-actions">
            <summary title="更多操作" aria-label="更多操作"><MoreHorizontal size={17} /></summary>
            <div>
              <button type="button" onClick={() => { setGithubError(''); setGithubOpen(true); }}><Link2 size={14} />从 GitHub 导入</button>
              <button type="button" onClick={onImportLocal}><FolderOpen size={14} />从本地导入</button>
            </div>
          </details>
          {view === 'installed' ? <button className="btn btn-primary" title="同步全部已安装技能到可用 AI 工具" onClick={onSyncAll} disabled={isBusy || !items.length}><RefreshCw className={busyAction === 'sync-all' ? 'spin' : ''} size={16} />{busyAction === 'sync-all' ? '同步中' : '同步全部'}</button> : null}
          {view === 'installed' ? <button className="btn btn-icon" title="打开技能目录" aria-label="打开技能目录" onClick={() => status?.target_root && onOpenDirectory(status.target_root)} disabled={!status?.target_root}><FolderOpen size={16} /></button> : null}
          <button className="btn btn-icon" title="刷新状态" aria-label="刷新状态" onClick={onRefresh} disabled={isBusy}><RefreshCw size={16} /></button>
        </>}
      />

      <SkillClientSummary status={status} mcpTargets={mcpTargets} onOpenAiConnections={onOpenAiConnections} />

      <section className="skill-workspace-target" aria-label="技能安装目标">
        <div><strong>{workspace.valid ? '当前项目' : '所有项目'}</strong><span title={workspace.valid ? workspace.agents_skills_root : status?.target_root}>{workspace.valid ? workspace.root : '使用全局技能目录'}{workspace.valid && status?.project_skills?.length ? ` · ${status.project_skills.length} 个项目技能` : ''}</span>{workspace.valid ? <small>由 HiMind 管理 {workspace.managed_skill_count} 个技能</small> : <small>未选择项目时，技能对所有项目可用</small>}</div>
        <div className="actions-row"><button className="btn" type="button" onClick={onPickWorkspace} disabled={isBusy}><FolderOpen size={14} />选择当前项目</button>{workspace.valid ? <button className="btn" type="button" onClick={onClearWorkspace} disabled={isBusy}>恢复全局</button> : null}</div>
      </section>

      {error ? <div className="blocker"><CircleAlert size={18} /><div><strong>技能状态读取失败</strong><span>{error}</span></div></div> : null}

      {externalSkillTargetsUnavailable(status) ? <div className="skill-inline-warning"><CircleAlert size={15} /><span>未发现可连接的其他 AI 工具。已安装技能仍可由 HiMind AI 使用。</span></div> : null}
      {workspace.valid && status?.project_skill_conflicts?.length ? <div className="skill-inline-warning"><CircleAlert size={15} /><span>发现 {status.project_skill_conflicts.length} 个项目技能冲突。同一技能同时存在 HiMind 版本和项目自带版本，请在项目中保留其中一份。</span></div> : null}
      {workspace.valid && status?.project_skills?.length ? <details className="skill-project-sources"><summary><span>项目技能来源</span><small>{status.project_skills.length} 个</small></summary><div>{status.project_skills.map(projectSkill => <div key={`${projectSkill.path}:${projectSkill.skill_id || projectSkill.name}`}><span><strong>{projectSkill.name}</strong><small>{projectSkill.management_mode === 'managed' ? 'HiMind 管理' : '项目自带'}</small></span><code>{projectSkill.path}</code></div>)}</div></details> : null}
	  <div className="plugin-toolbar skill-view-toolbar"><div className="plugin-tabs" role="tablist" aria-label="技能视图">
	    <button role="tab" aria-selected={view === 'installed'} className={view === 'installed' ? 'active' : ''} onClick={() => setView('installed')}>已安装 <span>{installedCount}</span></button>
        {dashboardEnabled ? <button role="tab" aria-selected={view === 'system'} className={view === 'system' ? 'active' : ''} onClick={() => setView('system')}>组织管理 <span>{systemSkillCount}</span></button> : null}
      </div><div className="skill-sync-compact"><span>文件方式</span><div className="segmented-control" role="group" aria-label="技能文件方式"><button type="button" title={workspace.valid ? '项目固定使用复制方式' : '复制文件'} aria-label="复制文件" aria-pressed={effectiveSyncMode === 'copy'} className={effectiveSyncMode === 'copy' ? 'active' : ''} disabled={isBusy || workspace.valid} onClick={() => onSetSyncMode('copy')}><Files size={13} />复制</button><button type="button" title={workspace.valid ? '项目不使用文件链接' : '链接文件'} aria-label="链接文件" aria-pressed={effectiveSyncMode === 'symlink'} className={effectiveSyncMode === 'symlink' ? 'active' : ''} disabled={isBusy || workspace.valid} onClick={() => onSetSyncMode('symlink')}><Link2 size={13} />链接</button></div>{workspace.valid ? <small className="skill-sync-note">项目固定复制</small> : null}</div></div>

      {dashboardEnabled && view === 'system' ? <ManagedCapabilitiesPanel assetKind="skill" desired={desired} loading={desiredLoading} error={desiredError} registry={pluginRegistry} skillStatus={status} /> : <section className={`skill-workspace compact-master-detail ${detailOpen ? 'detail-open' : ''}`}>
        <aside className="skill-browser">
          <div className="skill-browser-tools">
            <label className="skill-search"><Search size={15} /><input value={query} onChange={event => setQuery(event.target.value)} placeholder="搜索技能" /></label>
          </div>
          <div className="skill-browser-list">
			{filteredItems.map(item => <SkillListItem key={item.record.manifest.id} item={item} selected={item.record.manifest.id === selected?.record.manifest.id} onSelect={id => { setSelectedId(id); setDetailOpen(true); }} />)}
			{!filteredItems.length ? <EmptyState icon={BookOpen} title="没有匹配的技能" text="调整搜索内容或筛选条件。" /> : null}
          </div>
        </aside>

        <main className="skill-detail">
		  <button className="workspace-back" onClick={() => setDetailOpen(false)}><ArrowLeft size={15} />返回技能列表</button>
          {selected ? <SkillDetail item={selected} workspace={workspace} clientStatus={status} availablePlugins={availablePlugins} catalogPolicy={marketplace.find(item => item.skill_id === selected.record.manifest.id)} busyAction={busyAction} onLoadVersions={onLoadVersions} onPlanVersion={(version) => void openInstallPlan(selected.record.manifest.id, version)} onSync={onSyncSkill} onUpdateWorkspace={onUpdateWorkspace} onSetWorkspaceEnabled={onSetWorkspaceEnabled} onRepair={onRepair} onUninstall={() => setPendingUninstall(selected)} onSyncSkillClient={onSyncSkillClient} onUnregisterClient={onUnregisterClient} onUnregisterClients={onUnregisterClients} onOpenDirectory={onOpenDirectory} /> : <EmptyState icon={Sparkles} title="选择一个技能" text="查看功能、依赖和版本。" />}
        </main>
      </section>}

      {pendingUninstall ? <div className="skill-dialog-backdrop" role="presentation"><div className="skill-dialog" role="dialog" aria-modal="true" aria-labelledby="skill-uninstall-title">
        <div className="skill-dialog-head"><strong id="skill-uninstall-title">{workspace.valid ? '从项目移除技能' : '卸载技能'}</strong><button className="btn btn-icon" aria-label="关闭" onClick={() => setPendingUninstall(null)}><X size={16} /></button></div>
        <p>{workspace.valid ? <>将从当前项目中移除 <strong>{pendingUninstall.record.manifest.name}</strong>，全局技能和其他项目不受影响。</> : <>将卸载 <strong>{pendingUninstall.record.manifest.name}</strong>，并从已连接的 AI 工具中移除。</>}</p>
        {pendingUninstall.modified_files.length ? <div className="skill-dialog-warning"><CircleAlert size={16} />检测到用户修改。请先使用“修复并备份”保留当前文件。</div> : null}
        <div className="skill-dialog-actions"><button className="btn" onClick={() => setPendingUninstall(null)}>取消</button><button className="btn btn-danger" disabled={isBusy || pendingUninstall.modified_files.length > 0} onClick={() => { onUninstall(pendingUninstall.record.manifest.id); setPendingUninstall(null); }}><Trash2 size={15} />{workspace.valid ? '从项目移除' : '确认卸载'}</button></div>
      </div></div> : null}
	  {installPlan || planError ? <InstallPlanDialog plan={installPlan} error={planError} currentVersion={installPlan ? installedById.get(installPlan.skill.skill_id)?.record.manifest.version : undefined} busy={isBusy} onClose={() => { setInstallPlan(null); setPlanError(''); }} onInstall={(optionalIds) => { if (installPlan) onInstallMarketplace(installPlan.skill.skill_id, installPlan.skill.version, optionalIds); setInstallPlan(null); }} /> : null}
      {githubOpen ? <div className="modal-backdrop" role="presentation"><div className="modal" role="dialog" aria-modal="true" aria-labelledby="github-skill-title"><div className="modal-header"><div><h3 id="github-skill-title">从 GitHub 导入技能</h3><p>粘贴仓库链接。需要时可在链接中指定子目录和版本。</p></div><button className="btn btn-icon" aria-label="关闭" title="关闭" onClick={() => setGithubOpen(false)}><X size={16} /></button></div><div className="modal-body"><div className="field-group"><label className="field-label" htmlFor="github-skill-source-url">GitHub 链接</label><input id="github-skill-source-url" value={githubSourceUrl} onChange={event => setGithubSourceUrl(event.target.value)} placeholder="https://github.com/owner/repository.git?path=/skills/example#v1.0.0" /></div>{githubError ? <div className="inline-feedback visible" role="status">{githubError}</div> : null}<div className="modal-actions"><span /><div className="actions-row"><button className="btn" onClick={() => setGithubOpen(false)}>取消</button><button className="btn btn-primary" disabled={githubBusy || !githubSourceUrl.trim()} onClick={async () => { setGithubBusy(true); setGithubError(''); try { await onImportGithub(githubSourceUrl.trim()); setGithubOpen(false); } catch (error) { setGithubError(error instanceof Error ? error.message : 'GitHub 技能导入失败'); } finally { setGithubBusy(false); } }}>{githubBusy ? '导入中...' : '导入技能'}</button></div></div></div></div></div> : null}
    </div>
  );
}

const DISTRIBUTED_SKILL_STATES: CodexSkillStatusItem['client_state'][] = ['installed', 'outdated', 'modified', 'managed_elsewhere'];

const SKILL_CLIENT_NAMES: Record<string, string> = {
  'himind-ai': 'HiMind AI',
  codex: 'Codex',
  'github-copilot': 'GitHub Copilot',
  workbuddy: 'WorkBuddy',
  claude: 'Claude',
  cursor: 'Cursor',
  windsurf: 'Windsurf',
  cline: 'Cline',
  trae: 'Trae',
  codebuddy: 'CodeBuddy',
  qoder: 'Qoder',
  zcode: 'ZCode',
  antigravity: 'Antigravity',
  'gemini-cli': 'Gemini CLI',
  opencode: 'OpenCode',
  'kimi-code': 'Kimi Code',
  kiro: 'Kiro',
  'qwen-code': 'Qwen Code',
};

function skillClientDescriptors(status: CodexSkillStatusResponse | null, mcpTargets: McpTargetDescriptor[]) {
  const ids = new Set<string>(['himind-ai', 'codex']);
  Object.keys(status?.clients || {}).forEach(id => ids.add(id));
  mcpTargets.filter(target => target.supports_skills).forEach(target => ids.add(target.skill_client_id || target.id));
  const preferred = ['himind-ai', 'github-copilot', 'workbuddy', 'qoder', 'zcode', 'codex', 'claude'];
  return [...ids].filter(Boolean).sort((left, right) => {
    const li = preferred.indexOf(left); const ri = preferred.indexOf(right);
    if (li !== -1 || ri !== -1) return (li === -1 ? 99 : li) - (ri === -1 ? 99 : ri);
    return (SKILL_CLIENT_NAMES[left] || left).localeCompare(SKILL_CLIENT_NAMES[right] || right);
  }).map(id => {
    const clientStatus = statusForClient(status, id);
    return {
      id,
      name: clientStatus?.client_name || SKILL_CLIENT_NAMES[id] || id,
      detected: Boolean(clientStatus?.client_detected || clientStatus?.target_exists || (clientStatus?.target_configured && clientStatus?.target_kind !== 'workspace')),
      supportLevel: clientStatus?.support_level || (['himind-ai', 'codex'].includes(id) ? 'official' : 'compatible'),
      supportNote: clientStatus?.support_note || '',
    };
  });
}

function SkillClientIcon({ clientId, size = 16 }: { clientId: string; size?: number }) {
  if (clientId === 'himind-ai') return <Sparkles size={size} />;
  if (clientId === 'codex' || clientId === 'claude') return <Code2 size={size} />;
  if (clientId.includes('github')) return <span className="skill-client-letter">GH</span>;
  return <Wrench size={size} />;
}

function SkillClientSummary({ status, mcpTargets, onOpenAiConnections }: { status: CodexSkillStatusResponse | null; mcpTargets: McpTargetDescriptor[]; onOpenAiConnections: () => void }) {
  const clients = skillClientDescriptors(status, mcpTargets);
  const external = clients.filter(client => client.id !== 'himind-ai');
  const activeExternal = external.filter(client => client.detected || Boolean(targetForSkillClient(mcpTargets, client.id)?.detected));
  const attention = activeExternal.reduce((count, client) => {
    const state = statusForClient(status, client.id)?.items.some(item => ['outdated', 'modified', 'blocked', 'failed'].includes(item.client_state));
    return count + (state ? 1 : 0);
  }, 0);
  return <section className="skill-client-summary skill-client-summary-compact" aria-label="AI 工具技能状态">
    <div className="skill-client-summary-compact-main"><span className="status-dot success" /><div><small>技能状态</small><strong>HiMind AI 已就绪</strong><span>{activeExternal.length ? `已连接 ${activeExternal.length} 个其他 AI 工具` : '可按需连接其他 AI 工具'}{attention ? ` · ${attention} 个工具需处理` : ''}</span></div></div>
    <button type="button" className="btn btn-icon" title="管理 AI 连接" aria-label="管理 AI 连接" onClick={onOpenAiConnections}><Settings2 size={15} /></button>
  </section>;
}

function skillClientDistributionState(clientId: string, status?: CodexSkillStatusResponse): { label: string; detail: string; tone: 'success' | 'warn' | 'danger' | 'neutral' } {
  if (!status) return { label: '正在读取', detail: '技能分发状态', tone: 'neutral' };
  const items = status.items.filter(item => item.client_state !== 'unsupported');
  if (!items.length) return { label: '暂无适用技能', detail: '0 个技能', tone: 'neutral' };
  const distributed = items.filter(item => DISTRIBUTED_SKILL_STATES.includes(item.client_state)).length;
  const pending = items.filter(item => item.client_state === 'not_installed').length;
  const repair = items.filter(item => item.client_state === 'outdated' || item.client_state === 'modified').length;
  const blocked = items.filter(item => item.client_state === 'blocked' || item.client_state === 'failed').length;
  const foreign = items.filter(item => item.client_state === 'managed_elsewhere');
  const detail = `${distributed}/${items.length} 个技能`;
  if (blocked) return { label: `${blocked} 个技能不可用`, detail, tone: 'danger' };
  if (repair) return { label: `${repair} 个技能需处理`, detail, tone: 'warn' };
  if (pending) return { label: `${pending} 个技能待同步`, detail, tone: 'warn' };
  if (foreign.length === items.length) {
    return { label: '已同步', detail, tone: 'success' };
  }
  return { label: clientId === 'himind-ai' ? '技能直接可用' : '技能已同步', detail, tone: 'success' };
}

function targetForSkillClient(targets: McpTargetDescriptor[], clientId: string) {
  return targets
    .filter(target => (target.skill_client_id || target.id) === clientId)
    .sort((left, right) => Number(right.state === 'configured') - Number(left.state === 'configured') || Number(right.detected) - Number(left.detected))[0];
}

function statusForClient(status: CodexSkillStatusResponse | null, clientId: string) {
  if (!status) return undefined;
  return status.clients?.[clientId] || (clientId === 'codex' ? status : undefined);
}

function skillClientConnectionState(clientId: string, target?: McpTargetDescriptor): { label: string; tone: 'success' | 'warn' | 'danger' | 'neutral' } {
  if (clientId === 'himind-ai') return { label: '内置可用', tone: 'success' };
  if (!target) return { label: '暂不支持', tone: 'neutral' };
  if (target.state === 'configured') return { label: '已连接', tone: 'success' };
  if (target.state === 'needs_repair') return { label: '连接需更新', tone: 'warn' };
  if (target.state === 'invalid_config') return { label: '连接异常', tone: 'danger' };
  if (target.detected) return { label: '可连接', tone: 'neutral' };
  return { label: '未连接', tone: 'neutral' };
}

function externalSkillTargetsUnavailable(status: CodexSkillStatusResponse | null) {
  if (!status) return false;
  return Object.entries(status.clients || {}).filter(([clientId]) => clientId !== 'himind-ai').every(([, client]) => !client.client_detected && !client.target_exists && client.target_mode === 'preview');
}

function aggregateSkillStatusItems(status: CodexSkillStatusResponse | null): CodexSkillStatusItem[] {
  if (!status) return [];
  const clients = status.clients ? Object.values(status.clients) : [status];
  const records = new Map<string, CodexSkillStatusItem>();
  clients.forEach(client => client.items.forEach(item => {
    if (!records.has(item.record.manifest.id)) records.set(item.record.manifest.id, item);
  }));
  return [...records.values()].map(base => {
    const variants = clients
      .map(client => client.items.find(item => item.record.manifest.id === base.record.manifest.id))
      .filter((item): item is CodexSkillStatusItem => Boolean(item));
    const supported = variants.filter(item => item.client_state !== 'unsupported');
    const state = aggregateClientState(supported.map(item => item.client_state));
    const representative = supported.find(item => item.client_state === state) || supported[0] || base;
    const availableActions = new Set(supported.flatMap(item => item.available_actions));
    if (base.record.manifest.scope === 'builtin') availableActions.delete('uninstall');
    else availableActions.add('uninstall');
    return {
      ...representative,
      client_state: state,
      rendered: supported.some(item => item.rendered),
      rendered_valid: supported.some(item => item.rendered_valid),
      installed_version: supported.find(item => item.installed_version)?.installed_version || representative.installed_version,
      modified_files: [...new Set(supported.flatMap(item => item.modified_files))],
      available_actions: [...availableActions],
    };
  });
}

function aggregateClientState(states: CodexSkillStatusItem['client_state'][]): CodexSkillStatusItem['client_state'] {
  if (!states.length) return 'unsupported';
  for (const state of ['failed', 'modified', 'outdated', 'installed', 'managed_elsewhere', 'not_installed', 'blocked'] as const) {
    if (states.includes(state)) return state;
  }
  return 'unsupported';
}

function SkillDetailTabs({ value, onChange }: { value: 'details' | 'versions'; onChange: (value: 'details' | 'versions') => void }) {
  return <div className="extension-detail-tabs" role="tablist"><button role="tab" aria-selected={value === 'details'} className={value === 'details' ? 'active' : ''} onClick={() => onChange('details')}>详情</button><button role="tab" aria-selected={value === 'versions'} className={value === 'versions' ? 'active' : ''} onClick={() => onChange('versions')}>版本</button></div>;
}

type SkillVersionDisplay = Pick<OrganizationSkillCatalogItem, 'version' | 'release_notes' | 'published_at'>;

function SkillVersionList({ versions, currentVersion, lockedLabel, loading, error, onSelect }: { versions: SkillVersionDisplay[]; currentVersion?: string; lockedLabel?: string; loading: boolean; error: string; onSelect?: (version: string) => void }) {
  const sorted = [...versions].sort((left, right) => compareSemanticVersions(right.version, left.version));
  return <section className="extension-version-list">{loading ? <div className="extension-version-empty"><span className="spinner" />正在读取版本</div> : null}{error ? <div className="skill-inline-warning"><CircleAlert size={15} /><span>{error}</span></div> : null}{!loading && sorted.map(version => { const installed = currentVersion === version.version; const newer = currentVersion ? compareSemanticVersions(version.version, currentVersion) > 0 : false; const action = installed ? '已安装' : lockedLabel || (currentVersion ? newer ? '更新' : '切换' : '安装'); return <article className="extension-version-row" key={version.version}><div className="extension-version-main"><div><strong>v{version.version}</strong>{installed ? <Pill kind="success">已安装</Pill> : null}</div><time>{formatPublishedAt(version.published_at)}</time><p>{version.release_notes || '未提供更新说明。'}</p></div>{onSelect ? <button className={installed || lockedLabel ? 'btn' : 'btn btn-primary'} disabled={installed || Boolean(lockedLabel)} onClick={() => onSelect(version.version)}>{action}</button> : null}</article>; })}</section>;
}

function InstallPlanDialog({ plan, error, currentVersion, busy, onClose, onInstall }: { plan: SkillInstallPlan | null; error: string; currentVersion?: string; busy: boolean; onClose: () => void; onInstall: (optionalIds: string[]) => void }) {
  const [optionalIds, setOptionalIds] = useState<string[]>([]);
  const action = !plan || !currentVersion ? '安装' : compareSemanticVersions(plan.skill.version, currentVersion) > 0 ? '更新' : '切换版本';
  return <div className="skill-dialog-backdrop"><div className="skill-dialog skill-plan-dialog" role="dialog" aria-modal="true"><div className="skill-dialog-head"><strong>{action === '切换版本' ? '切换技能版本' : `${action}技能`}</strong><button className="btn btn-icon" onClick={onClose} aria-label="关闭"><X size={16} /></button></div>{error ? <div className="skill-dialog-warning"><CircleAlert size={16} />{error}</div> : null}{plan ? <><div className="skill-plan-summary"><strong>{plan.skill.name} v{plan.skill.version}</strong><span>{plan.ready ? '可以安装' : '当前无法安装'}</span></div><div className="skill-plan-actions">{plan.plugin_actions.map(action => <label key={action.plugin_id} className={action.action === 'blocked' || action.action === 'unavailable' ? 'blocked' : ''}><input type="checkbox" checked={action.required || optionalIds.includes(action.plugin_id)} disabled={action.required || !['install', 'update'].includes(action.action)} onChange={event => setOptionalIds(current => event.target.checked ? [...current, action.plugin_id] : current.filter(id => id !== action.plugin_id))} /><span><strong>{action.plugin_name || readablePluginID(action.plugin_id)}</strong><small>{installActionDescription(action.action)}</small></span><code>{action.target_version ? `v${action.target_version}` : '--'}</code></label>)}</div></> : null}<div className="skill-dialog-actions"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-primary" disabled={!plan?.ready || busy} onClick={() => onInstall(optionalIds)}><Download size={15} />确认{action}</button></div></div></div>;
}

function PluginDependencyIdentity({ pluginId, plugins }: { pluginId: string; plugins: PluginCatalogItem[] }) {
  const plugin = plugins.find(item => item.plugin_id === pluginId);
  return <span className="skill-dependency-identity"><strong>{plugin?.name || readablePluginID(pluginId)}</strong></span>;
}

function SkillListItem({ item, selected, onSelect }: { item: CodexSkillStatusItem; selected: boolean; onSelect: (id: string) => void }) {
  const manifest = item.record.manifest;
  return <button className={`skill-browser-item ${selected ? 'selected' : ''}`} onClick={() => onSelect(manifest.id)}><span className={`skill-state-rail ${stateTone(item.client_state)}`} /><span className="skill-browser-item-copy"><strong>{manifest.name}</strong><small>作者：{manifest.author || '未知作者'}</small><small>{manifest.description || manifest.id}</small></span><span className={`skill-state-label ${stateTone(item.client_state)}`}>{clientStateLabel(item.client_state)}</span></button>;
}

function SkillDetail({ item, workspace, clientStatus, availablePlugins, catalogPolicy, busyAction, onLoadVersions, onPlanVersion, onSync, onUpdateWorkspace, onSetWorkspaceEnabled, onRepair, onUninstall, onSyncSkillClient, onUnregisterClient, onUnregisterClients, onOpenDirectory }: { item: CodexSkillStatusItem; workspace: SkillWorkspaceStatus; clientStatus: CodexSkillStatusResponse | null; availablePlugins: PluginCatalogItem[]; catalogPolicy?: OrganizationSkillCatalogItem; busyAction: string | null; onLoadVersions: (skillId: string) => Promise<OrganizationSkillCatalogItem[]>; onPlanVersion: (version: string) => void; onSync: (id: string) => void; onUpdateWorkspace: (id: string) => void; onSetWorkspaceEnabled: (id: string, enabled: boolean) => void; onRepair: (id: string) => void; onUninstall: () => void; onSyncSkillClient: (skillId: string, clientId: string) => void; onUnregisterClient: (skillId: string, clientId: string) => void; onUnregisterClients: (skillId: string) => void; onOpenDirectory: (path: string) => void }) {
  const manifest = item.record.manifest;
  const actionBusy = Boolean(busyAction?.endsWith(manifest.id));
  const [tab, setTab] = useState<'details' | 'versions'>('details');
  const [versions, setVersions] = useState<SkillVersionDisplay[]>(catalogPolicy ? [catalogPolicy] : [{ version: manifest.version, release_notes: manifest.release_notes || '' }]);
  const [versionsLoading, setVersionsLoading] = useState(false);
  const [versionsError, setVersionsError] = useState('');
  const managed = catalogPolicy?.management !== 'user_managed' && Boolean(catalogPolicy?.managed);
  const workspaceManaged = workspace.managed_skills?.find(entry => entry.skill_id === manifest.id);
  const workspaceUpdateBusy = busyAction === `workspace-update:${manifest.id}`;
  const workspaceEnabledBusy = busyAction === `workspace-enabled:${manifest.id}`;

  useEffect(() => {
    if (tab !== 'versions' || !catalogPolicy) return;
    let active = true;
    setVersionsLoading(true);
    setVersionsError('');
    onLoadVersions(manifest.id).then(result => { if (active) setVersions(result); }).catch(() => { if (active) setVersionsError('暂时无法读取版本，请稍后重试。'); }).finally(() => { if (active) setVersionsLoading(false); });
    return () => { active = false; };
  }, [catalogPolicy, manifest.id, onLoadVersions, tab]);
  return <>
    <header className="skill-detail-header"><div className="skill-detail-title"><span className="skill-detail-mark">{manifest.name.slice(0, 1).toUpperCase()}</span><div><div className="skill-title-line"><h3>{manifest.name}</h3><Pill kind={statePill(item.client_state)}>{clientStateLabel(item.client_state)}</Pill></div><small className="skill-detail-source">作者：{manifest.author || '未知作者'}</small></div></div><div className="skill-detail-actions">
      {workspace.valid && item.update_available ? <button className="btn btn-primary" title={`当前项目锁定 v${item.pinned_version || ''}，技能库已是 v${item.available_version}`} disabled={workspaceUpdateBusy || Boolean(busyAction)} onClick={() => onUpdateWorkspace(manifest.id)}><RefreshCw className={workspaceUpdateBusy ? 'spin' : ''} size={15} />更新当前项目到 v{item.available_version}</button> : null}
      {item.available_actions.includes('repair') ? <button className="btn" disabled={actionBusy} onClick={() => onRepair(manifest.id)}><Wrench size={15} />{item.client_state === 'modified' ? '修复并备份' : '重新同步'}</button> : null}
      {item.available_actions.includes('uninstall') && catalogPolicy?.allow_uninstall !== false ? <button className="btn btn-danger-quiet" disabled={actionBusy} onClick={onUninstall}><Trash2 size={15} />卸载</button> : null}
    </div></header>
    <SkillDetailTabs value={tab} onChange={setTab} />
    {item.readiness.state !== 'ready' ? <div className="skill-detail-notice"><CircleAlert size={16} /><div><strong>{item.readiness.state === 'blocked' ? '当前不可安装' : '部分功能不可用'}</strong><span>请安装所需插件或更新 HiMind Agent。</span></div></div> : null}
    {item.client_state === 'modified' ? <div className="skill-detail-notice modified"><Wrench size={16} /><div><strong>检测到技能文件已被修改</strong><span>修复前会自动保留一份备份。</span></div></div> : null}
    {tab === 'versions' ? <SkillVersionList versions={versions} currentVersion={item.installed_version || manifest.version} lockedLabel={catalogPolicy?.assignment === 'blocked' ? '不可安装' : managed ? '由组织管理' : undefined} loading={versionsLoading} error={versionsError} onSelect={catalogPolicy ? onPlanVersion : undefined} /> : <>
      <section className="skill-detail-section"><div className="skill-section-title"><div><strong>功能</strong></div></div><p className="skill-release-notes">{manifest.description || '暂无功能说明。'}</p></section>
      <section className="skill-detail-section"><div className="skill-section-title"><div><strong>AI 工具</strong><small>选择可以使用此技能的工具</small></div></div><SkillClientAvailability status={clientStatus} skillId={manifest.id} busyAction={busyAction} canRegisterAll={item.available_actions.includes('install') || item.available_actions.includes('update')} allowUnregister={catalogPolicy?.allow_uninstall !== false} onRegisterAll={onSync} onSyncSkillClient={onSyncSkillClient} onUnregisterClient={onUnregisterClient} onUnregisterClients={onUnregisterClients} /></section>
      {workspace.valid && workspaceManaged ? <section className="skill-detail-section"><div className="skill-section-title"><div><strong>当前项目管理</strong><small>锁定 v{workspaceManaged.version} · {workspaceManaged.enabled ? '已启用' : '已禁用'}</small></div><button type="button" className="btn" disabled={workspaceEnabledBusy || Boolean(busyAction)} onClick={() => onSetWorkspaceEnabled(manifest.id, !workspaceManaged.enabled)}>{workspaceManaged.enabled ? '在项目中停用' : '在项目中启用'}</button></div><p className="skill-release-notes">当前项目使用锁定版本，技能库更新不会自动修改项目。停用后会从当前项目移除该技能，重新启用即可恢复同步。</p></section> : null}
      <section className="skill-detail-section"><div className="skill-section-title"><div><strong>依赖</strong></div></div><div className="skill-dependency-list">{(manifest.plugin_dependencies || []).map(dependency => <div key={dependency.plugin_id}><span className="status-dot success" /><PluginDependencyIdentity pluginId={dependency.plugin_id} plugins={availablePlugins} /><span>{dependency.required ? '必需' : '可选'}</span><strong>{dependency.min_version ? `v${dependency.min_version} 及以上` : '不限版本'}</strong></div>)}{!manifest.plugin_dependencies?.length ? <span className="skill-section-empty">无依赖</span> : null}</div></section>
      <details className="plugin-technical-panel"><summary>开发者信息</summary><div className="plugin-technical-grid"><div><span>技能 ID</span><code>{manifest.id}</code></div><div><span>作者</span><strong>{manifest.author || '未知作者'}</strong></div><div><span>来源</span><strong>{scopeLabel(manifest.scope)}</strong></div><div><span>最近同步</span><strong>{formatSyncedAt(item.last_synced_at)}</strong></div><div className="wide"><span>本地目录</span><code>{item.rendered_root || '--'}</code></div></div><div className="skill-file-summary"><span /><button className="text-action" disabled={!item.rendered} onClick={() => onOpenDirectory(item.rendered_root)}><FolderOpen size={14} />打开目录</button></div></details>
    </>}
  </>;
}

function SkillClientAvailability({ status, skillId, busyAction, canRegisterAll, allowUnregister, onRegisterAll, onSyncSkillClient, onUnregisterClient, onUnregisterClients }: { status: CodexSkillStatusResponse | null; skillId: string; busyAction: string | null; canRegisterAll: boolean; allowUnregister: boolean; onRegisterAll: (skillId: string) => void; onSyncSkillClient: (skillId: string, clientId: string) => void; onUnregisterClient: (skillId: string, clientId: string) => void; onUnregisterClients: (skillId: string) => void }) {
  const clients = skillClientDescriptors(status, []);
  const relevant = clients.filter(client => client.id === 'himind-ai' || client.detected || DISTRIBUTED_SKILL_STATES.includes(statusForClient(status, client.id)?.items.find(candidate => candidate.record.manifest.id === skillId)?.client_state || 'not_installed'));
  const external = relevant.filter(client => client.id !== 'himind-ai');
  const skillItems = new Map(external.map(client => [client.id, statusForClient(status, client.id)?.items.find(candidate => candidate.record.manifest.id === skillId)]));
  const synced = external.filter(client => ['installed', 'outdated', 'modified', 'managed_elsewhere'].includes(skillItems.get(client.id)?.client_state || '')).length;
  const pending = external.filter(client => skillItems.get(client.id)?.client_state === 'not_installed').length;
  const attention = external.filter(client => ['outdated', 'modified', 'blocked', 'failed'].includes(skillItems.get(client.id)?.client_state || '')).length;
  const registerAllBusy = busyAction === `sync:${skillId}`;
  const renderClient = (client: (typeof clients)[number]) => {
    const item = statusForClient(status, client.id)?.items.find(candidate => candidate.record.manifest.id === skillId);
    const state = skillAvailabilityState(client.id, item);
    const canUnregister = allowUnregister && client.id !== 'himind-ai' && ['installed', 'outdated'].includes(item?.client_state || '');
    const unregisterBusy = busyAction === `unregister:${client.id}:${skillId}`;
    const registerable = client.id !== 'himind-ai' && ['not_installed', 'outdated', 'modified'].includes(item?.client_state || '') && client.detected;
    const registerBusy = busyAction === `register:${client.id}:${skillId}`;
    return <div className="skill-client-tool" key={client.id}><span className={`skill-client-mini-icon ${client.id === 'himind-ai' ? 'himind-ai' : ''}`}><SkillClientIcon clientId={client.id} size={14} /></span><span className="skill-client-tool-copy"><strong title={client.name}>{client.name}</strong><small className={state.tone}>{state.label}</small></span>{canUnregister ? <button type="button" className="btn btn-icon btn-danger-quiet skill-client-unregister" title={`停止同步到 ${client.name}`} aria-label={`停止同步到 ${client.name}`} disabled={unregisterBusy} onClick={() => onUnregisterClient(skillId, client.id)}><Link2Off className={unregisterBusy ? 'spin' : ''} size={13} /></button> : registerable ? <button type="button" className="btn btn-icon skill-client-register" title={`同步到 ${client.name}`} aria-label={`同步到 ${client.name}`} disabled={registerBusy} onClick={() => onSyncSkillClient(skillId, client.id)}><PlugZap className={registerBusy ? 'spin' : ''} size={13} /></button> : <span className="skill-client-tool-action-space" />}</div>;
  };
  if (!relevant.length) return <div className="skill-client-availability"><span className="skill-section-empty">暂无可用的 AI 工具</span></div>;
  const summary = status ? `已同步到 ${synced} 个工具${pending ? ` · ${pending} 个未同步` : ''}${attention ? ` · ${attention} 个需处理` : ''}` : '正在读取同步状态';
  const canUnregisterAll = allowUnregister && external.some(client => ['installed', 'outdated', 'managed_elsewhere'].includes(skillItems.get(client.id)?.client_state || ''));
  return <div className="skill-client-availability"><div className="skill-client-distribution-summary"><span className="status-dot success" /><span><strong>HiMind AI 已就绪</strong><small>{summary}</small></span><div className="skill-client-summary-actions">{canRegisterAll ? <button type="button" className="btn btn-icon btn-primary" title="同步到全部工具" aria-label="同步到全部工具" disabled={Boolean(busyAction)} onClick={() => onRegisterAll(skillId)}><PlugZap className={registerAllBusy ? 'spin' : ''} size={14} /></button> : null}{canUnregisterAll ? <button type="button" className="btn btn-icon btn-danger-quiet" title="停止全部同步" aria-label="停止全部同步" disabled={Boolean(busyAction)} onClick={() => onUnregisterClients(skillId)}><Link2Off size={14} /></button> : null}</div></div><details className="skill-client-tools"><summary><span><Settings2 size={14} /><strong>按工具管理</strong><small>{relevant.length}</small></span><ChevronDown className="skill-client-tools-chevron" size={15} /></summary><div>{relevant.map(renderClient)}</div></details></div>;
}

function skillAvailabilityState(clientId: string, item?: CodexSkillStatusItem): { label: string; tone: 'success' | 'warn' | 'danger' | 'neutral' } {
  const state = item?.client_state;
  if (!state || state === 'unsupported') return { label: '不支持', tone: 'neutral' };
  if (state === 'installed') return { label: clientId === 'himind-ai' ? '直接可用' : '已同步', tone: 'success' };
  if (state === 'managed_elsewhere') return { label: '由其他安装管理', tone: 'success' };
  if (state === 'not_installed') return { label: '未同步', tone: 'neutral' };
  if (state === 'outdated') return { label: '待更新', tone: 'warn' };
  if (state === 'modified') return { label: '已修改', tone: 'warn' };
  if (state === 'blocked') return { label: '依赖未满足', tone: 'danger' };
  return { label: '同步失败', tone: 'danger' };
}

function isManagedPolicy(item: ExtensionDesiredItem) {
  return item.management !== 'user_managed' || item.intent === 'required' || item.desired_state === 'absent';
}

function isManagedCatalogSkill(item: OrganizationSkillCatalogItem) {
  return item.source === 'system' || item.management === 'builtin' || item.management === 'organization_managed' || ['required', 'blocked'].includes(item.assignment || '');
}

function isManagedSkill(item: CodexSkillStatusItem, desired: ExtensionDesiredState | null, marketplace: OrganizationSkillCatalogItem[]) {
  const skillId = item.record.manifest.id;
  if (item.record.manifest.scope === 'builtin') return true;
  if (desired?.items.some(policy => policy.asset_kind === 'skill' && policy.asset_key === skillId && isManagedPolicy(policy))) return true;
  return marketplace.some(policy => policy.skill_id === skillId && isManagedCatalogSkill(policy));
}

function clientStateLabel(state: CodexSkillStatusItem['client_state']) {
  const labels: Record<CodexSkillStatusItem['client_state'], string> = { not_installed: '未安装', installed: '已安装', outdated: '有更新', modified: '已修改', managed_elsewhere: '其他实例已同步', blocked: '不可用', unsupported: '不兼容', failed: '失败' };
  return labels[state] || state;
}

function stateTone(state: CodexSkillStatusItem['client_state']) { if (state === 'installed' || state === 'managed_elsewhere') return 'success'; if (state === 'outdated' || state === 'modified') return 'warn'; if (state === 'not_installed') return 'neutral'; return 'danger'; }
function statePill(state: CodexSkillStatusItem['client_state']): 'success' | 'warn' | 'danger' { if (state === 'installed' || state === 'managed_elsewhere') return 'success'; if (state === 'outdated' || state === 'modified' || state === 'not_installed') return 'warn'; return 'danger'; }
function scopeLabel(scope: string) { if (scope === 'builtin') return '系统内置'; if (scope === 'organization') return '技能市场'; if (scope === 'user') return '我的技能'; return scope || '--'; }
function formatSyncedAt(value?: string | null) { if (!value) return '尚未同步'; const milliseconds = Number.parseInt(value.split('-')[0], 10); return Number.isFinite(milliseconds) ? new Date(milliseconds).toLocaleString('zh-CN', { hour12: false }) : value; }
function formatPublishedAt(value?: string) { if (!value) return '发布时间未知'; const date = new Date(value); return Number.isNaN(date.getTime()) ? value : date.toLocaleDateString('zh-CN'); }
function installActionDescription(action: string) { return ({ satisfied: '已安装', install: '将一并安装', update: '将一并更新', blocked: '被组织策略阻止', unavailable: '当前不可用' } as Record<string, string>)[action] || '需要处理'; }
function readablePluginID(value: string) { const tail = value.split('.').filter(Boolean).pop() || value; return tail.split(/[-_]/).filter(Boolean).map(part => part.charAt(0).toUpperCase() + part.slice(1)).join(' '); }
function compareSemanticVersions(left: string, right: string) {
  const parse = (value: string) => value.split(/[.+-]/).slice(0, 3).map(part => Number.parseInt(part, 10) || 0);
  const a = parse(left);
  const b = parse(right);
  for (let index = 0; index < 3; index += 1) {
    if ((a[index] || 0) !== (b[index] || 0)) return (a[index] || 0) - (b[index] || 0);
  }
  return 0;
}
