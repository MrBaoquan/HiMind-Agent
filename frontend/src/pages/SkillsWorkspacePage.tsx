import { useEffect, useMemo, useState } from 'react';
import {
  BookOpen,
  BadgeCheck,
  Building2,
  CheckCircle2,
  CircleAlert,
  FolderOpen,
  Download,
  PackagePlus,
  Edit3,
  Play,
  Plus,
  Send,
  RefreshCw,
  Files,
  Link2,
  Search,
  ShieldCheck,
  Sparkles,
  Trash2,
  Wrench,
  X,
} from 'lucide-react';
import { EmptyState, PageHeader, Pill, Tags } from '../components/Common';
import type { AuthoringSkillDraft, AuthoringSkillDraftInput, CodexSkillStatusItem, CodexSkillStatusResponse, OrganizationSkillCatalogItem, PluginCatalogItem, SkillCatalogResponse, SkillInstallPlan, SkillSubmissionStatus, SkillSyncSettings } from '../services/agentApi';
import { FUNCTIONAL_CATEGORIES, categorySearchText, functionalCategoryID, functionalCategoryLabels, functionalCategoryMatches } from '../data/categoryCatalog';

type SkillsWorkspacePageProps = {
  catalog: SkillCatalogResponse | null;
  status: CodexSkillStatusResponse | null;
  error: string | null;
  marketplace: OrganizationSkillCatalogItem[];
  marketplaceError: string | null;
	availablePlugins: PluginCatalogItem[];
  drafts: AuthoringSkillDraft[];
  submissions: SkillSubmissionStatus[];
  busyAction: string | null;
  onRefresh: () => void;
  onSyncAll: () => void;
  onSyncSkill: (skillId: string) => void;
  syncMode: SkillSyncSettings['mode'];
  onSetSyncMode: (mode: SkillSyncSettings['mode']) => void;
  onLoadVersions: (skillId: string) => Promise<OrganizationSkillCatalogItem[]>;
  onPlanMarketplace: (skillId: string, version?: string) => Promise<SkillInstallPlan>;
  onInstallMarketplace: (skillId: string, version: string | undefined, optionalPluginIds: string[]) => void;
  onSaveDraft: (input: AuthoringSkillDraftInput) => Promise<AuthoringSkillDraft>;
  authorName: string;
  onImportCandidate: (revisionOfVersion?: string, parentSubmissionId?: string) => void;
  onCreateRevision: (skillId: string, version: string) => void;
  onTestDraft: (skillId: string, version: string) => void;
  onConfirmDraft: (skillId: string, version: string) => void;
  onSubmitDraft: (skillId: string, version: string) => void;
  onRepair: (skillId: string) => void;
  onUninstall: (skillId: string) => void;
  onOpenDirectory: (path: string) => void;
};

type ViewKey = 'marketplace' | 'installed' | 'mine';

export function SkillsWorkspacePage({ catalog, status, error, marketplace, marketplaceError, availablePlugins, drafts, submissions, busyAction, syncMode, authorName, onSetSyncMode, onRefresh, onSyncAll, onSyncSkill, onLoadVersions, onPlanMarketplace, onInstallMarketplace, onSaveDraft, onImportCandidate, onCreateRevision, onTestDraft, onConfirmDraft, onSubmitDraft, onRepair, onUninstall, onOpenDirectory }: SkillsWorkspacePageProps) {
  const [query, setQuery] = useState('');
  const [categoryFilter, setCategoryFilter] = useState('all');
  const [view, setView] = useState<ViewKey>('marketplace');
  const [selectedId, setSelectedId] = useState('');
  const [pendingUninstall, setPendingUninstall] = useState<CodexSkillStatusItem | null>(null);
  const [editingDraft, setEditingDraft] = useState<AuthoringSkillDraft | null>(null);
  const [draftEditorOpen, setDraftEditorOpen] = useState(false);
  const [installPlan, setInstallPlan] = useState<SkillInstallPlan | null>(null);
  const [planError, setPlanError] = useState('');
  const [planLoading, setPlanLoading] = useState(false);
  const items = status?.items || [];
	const installedById = useMemo(() => new Map(items.filter(item => ['installed', 'outdated', 'modified'].includes(item.client_state)).map(item => [item.record.manifest.id, item])), [items]);
	const localItems = useMemo(() => items.filter(item => view === 'mine' ? item.record.manifest.scope === 'user' : item.client_state !== 'not_installed'), [items, view]);
	const categoryCounts = useMemo(() => new Map(FUNCTIONAL_CATEGORIES.map(category => [category.id, marketplace.filter(item => functionalCategoryMatches(item.categories, category.id)).length])), [marketplace]);
	const visibleMarket = useMemo(() => {
	  const normalized = query.trim().toLowerCase();
	  return marketplace.filter(item => {
	    if (normalized && ![item.skill_id, item.name, item.description, item.author_name, categorySearchText(item.categories), ...item.capability_ids].join(' ').toLowerCase().includes(normalized)) return false;
	    if (categoryFilter !== 'all' && !functionalCategoryMatches(item.categories, categoryFilter)) return false;
	    return true;
	  });
	}, [categoryFilter, marketplace, query]);
	const visibleDrafts = useMemo(() => {
	  const normalized = query.trim().toLowerCase();
	  return drafts.filter(draft => !normalized || [draft.manifest.id, draft.manifest.name, draft.manifest.description].join(' ').toLowerCase().includes(normalized));
	}, [drafts, query]);
	const remoteOnlySubmissions = useMemo(() => submissions.filter(submission => !drafts.some(draft => draft.dashboard_draft_id === submission.id || (draft.manifest.id === submission.product_key && draft.manifest.version === submission.version))), [drafts, submissions]);
	const visibleRemoteSubmissions = useMemo(() => { const normalized = query.trim().toLowerCase(); return remoteOnlySubmissions.filter(item => !normalized || [item.product_key, item.name, item.author_name].filter(Boolean).join(' ').toLowerCase().includes(normalized)); }, [query, remoteOnlySubmissions]);

  useEffect(() => {
    const selectableIds = view === 'marketplace' ? marketplace.map(item => item.skill_id) : view === 'mine' ? [...drafts.map(draftKey), ...remoteOnlySubmissions.map(item => `remote:${item.id}`)] : localItems.map(item => item.record.manifest.id);
    if (!selectableIds.length) {
      setSelectedId('');
    } else if (!selectableIds.includes(selectedId)) {
      setSelectedId(selectableIds[0]);
    }
  }, [view, marketplace, localItems, drafts, remoteOnlySubmissions, selectedId]);

  const filteredItems = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    return localItems.filter(item => {
      if (!normalized) return true;
      const manifest = item.record.manifest;
      return [manifest.id, manifest.name, manifest.description, manifest.risk_summary, ...(manifest.capabilities || []).map(capability => capability.id)].join(' ').toLowerCase().includes(normalized);
    });
  }, [localItems, query]);

  const selected = filteredItems.find(item => item.record.manifest.id === selectedId) || filteredItems[0];
	const selectedMarket = visibleMarket.find(item => item.skill_id === selectedId) || visibleMarket[0];
	const selectedDraft = visibleDrafts.find(item => draftKey(item) === selectedId) || (!selectedId.startsWith('remote:') ? visibleDrafts[0] : undefined);
	const selectedRemoteSubmission = visibleRemoteSubmissions.find(item => `remote:${item.id}` === selectedId) || (!selectedDraft ? visibleRemoteSubmissions[0] : undefined);
	const selectedSubmission = selectedDraft ? submissions.find(item => item.id === selectedDraft.dashboard_draft_id || (item.product_key === selectedDraft.manifest.id && item.version === selectedDraft.manifest.version)) : undefined;
  const installedCount = items.filter(item => ['installed', 'outdated', 'modified'].includes(item.client_state)).length;
  const isBusy = Boolean(busyAction);

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
        title="AI 技能"
        description="安装和管理 AI 技能"
        actions={<>
          {view === 'installed' ? <button className="btn btn-primary" onClick={onSyncAll} disabled={isBusy || !items.length}><RefreshCw className={busyAction === 'sync-all' ? 'spin' : ''} size={16} />{busyAction === 'sync-all' ? '正在更新' : '更新全部'}</button> : null}
          {view === 'installed' ? <button className="btn btn-icon" title="打开技能目录" aria-label="打开技能目录" onClick={() => status?.target_root && onOpenDirectory(status.target_root)} disabled={!status?.target_root}><FolderOpen size={16} /></button> : null}
          <button className="btn btn-icon" title="刷新状态" aria-label="刷新状态" onClick={onRefresh} disabled={isBusy}><RefreshCw size={16} /></button>
        </>}
      />

      <SkillClientSummary status={status} />

      {error ? <div className="blocker"><CircleAlert size={18} /><div><strong>技能状态读取失败</strong><span>{error}</span></div></div> : null}

      {status?.target_mode === 'preview' && view !== 'marketplace' ? <div className="skill-inline-warning"><CircleAlert size={15} /><span>未找到 AI 工具的技能目录，当前安装仅在 HiMind Agent 中可用。</span></div> : null}
	  {marketplaceError && view === 'marketplace' ? <div className="skill-inline-warning"><CircleAlert size={15} /><span>{marketplaceError}</span></div> : null}
	  <div className="plugin-toolbar skill-view-toolbar"><div className="plugin-tabs" role="tablist" aria-label="技能视图">
	    <button role="tab" aria-selected={view === 'marketplace'} className={view === 'marketplace' ? 'active' : ''} onClick={() => setView('marketplace')}>市场 <span>{marketplace.length}</span></button>
	    <button role="tab" aria-selected={view === 'installed'} className={view === 'installed' ? 'active' : ''} onClick={() => setView('installed')}>已安装 <span>{installedCount}</span></button>
	  </div></div>
	  {view === 'installed' ? <details className="skill-sync-settings"><summary><span><strong>高级文件设置</strong><small>当前使用{syncMode === 'symlink' ? '软链接' : '复制文件'}</small></span></summary><div className="segmented-control" role="group" aria-label="技能文件管理方式"><button type="button" className={syncMode === 'copy' ? 'active' : ''} disabled={isBusy} onClick={() => onSetSyncMode('copy')}><Files size={14} />复制文件</button><button type="button" className={syncMode === 'symlink' ? 'active' : ''} disabled={isBusy} onClick={() => onSetSyncMode('symlink')}><Link2 size={14} />软链接</button></div></details> : null}

      <section className="skill-workspace">
        <aside className="skill-browser">
          <div className="skill-browser-tools">
            <label className="skill-search"><Search size={15} /><input value={query} onChange={event => setQuery(event.target.value)} placeholder="搜索技能" /></label>
            {view === 'marketplace' ? <div className="market-category-block"><div className="market-category-heading"><strong>功能分类</strong><span>按用途查找</span></div><label className="market-category-select"><span className="sr-only">技能功能分类</span><select value={categoryFilter} onChange={event => setCategoryFilter(event.target.value)}><option value="all">全部技能（{marketplace.length}）</option>{FUNCTIONAL_CATEGORIES.map(category => <option value={category.id} key={category.id}>{category.label}（{categoryCounts.get(category.id) || 0}）</option>)}</select></label><nav className="market-category-nav" aria-label="技能功能分类"><button type="button" className={categoryFilter === 'all' ? 'active' : ''} onClick={() => setCategoryFilter('all')}>全部技能<span>{marketplace.length}</span></button>{FUNCTIONAL_CATEGORIES.map(category => <button type="button" key={category.id} className={categoryFilter === category.id ? 'active' : ''} onClick={() => setCategoryFilter(category.id)}>{category.label}<span>{categoryCounts.get(category.id) || 0}</span></button>)}</nav></div> : null}
          </div>
          <div className="skill-browser-list">
			{view === 'marketplace' ? visibleMarket.map(item => <MarketSkillListItem key={item.skill_id} item={item} installed={installedById.get(item.skill_id)} selected={item.skill_id === selectedMarket?.skill_id} onSelect={setSelectedId} />) : view === 'mine' ? <>{visibleDrafts.map(item => <DraftListItem key={draftKey(item)} item={item} submission={submissions.find(status => status.id === item.dashboard_draft_id)} selected={draftKey(item) === draftKey(selectedDraft)} onSelect={setSelectedId} />)}{visibleRemoteSubmissions.map(item => <RemoteSkillSubmissionListItem key={item.id} item={item} selected={`remote:${item.id}` === selectedId} onSelect={setSelectedId} />)}</> : filteredItems.map(item => <SkillListItem key={item.record.manifest.id} item={item} selected={item.record.manifest.id === selected?.record.manifest.id} onSelect={setSelectedId} />)}
			{(view === 'marketplace' ? !visibleMarket.length : view === 'mine' ? !visibleDrafts.length && !visibleRemoteSubmissions.length : !filteredItems.length) ? <EmptyState icon={view === 'marketplace' ? Building2 : BookOpen} title={view === 'marketplace' ? '技能库暂无内容' : view === 'mine' ? '还没有创作技能' : '没有匹配的技能'} text={view === 'marketplace' ? '审核通过并正式发布的 AI 技能会出现在这里。' : view === 'mine' ? '创建技能并完成实际测试后，可以提交组织审核。' : '调整搜索内容或筛选条件。'} /> : null}
          </div>
        </aside>

        <main className="skill-detail">
		  {view === 'marketplace' ? (selectedMarket ? <MarketSkillDetail item={selectedMarket} installed={installedById.get(selectedMarket.skill_id)} availablePlugins={availablePlugins} busyAction={busyAction} onLoadVersions={onLoadVersions} onPlan={(version) => void openInstallPlan(selectedMarket.skill_id, version)} planLoading={planLoading} /> : <EmptyState icon={Sparkles} title="选择一个技能" text="查看功能、依赖和版本。" />) : view === 'mine' ? (selectedDraft ? <DraftDetail item={selectedDraft} availablePlugins={availablePlugins} submission={selectedSubmission} busyAction={busyAction} onCreateRevision={onCreateRevision} onEdit={() => { setEditingDraft(selectedDraft); setDraftEditorOpen(true); }} onTest={onTestDraft} onConfirm={onConfirmDraft} onSubmit={onSubmitDraft} onOpenDirectory={onOpenDirectory} /> : selectedRemoteSubmission ? <RemoteSkillSubmissionDetail item={selectedRemoteSubmission} onImportCandidate={onImportCandidate} /> : <EmptyState icon={Sparkles} title="创建本地技能" text="完成编辑、测试后提交审核。" />) : (selected ? <SkillDetail item={selected} availablePlugins={availablePlugins} catalogPolicy={marketplace.find(item => item.skill_id === selected.record.manifest.id)} busyAction={busyAction} onLoadVersions={onLoadVersions} onPlanVersion={(version) => void openInstallPlan(selected.record.manifest.id, version)} onSync={onSyncSkill} onRepair={onRepair} onUninstall={() => setPendingUninstall(selected)} onOpenDirectory={onOpenDirectory} /> : <EmptyState icon={Sparkles} title="选择一个技能" text="查看功能、依赖和版本。" />)}
        </main>
      </section>

      {pendingUninstall ? <div className="skill-dialog-backdrop" role="presentation"><div className="skill-dialog" role="dialog" aria-modal="true" aria-labelledby="skill-uninstall-title">
        <div className="skill-dialog-head"><strong id="skill-uninstall-title">卸载技能</strong><button className="btn btn-icon" aria-label="关闭" onClick={() => setPendingUninstall(null)}><X size={16} /></button></div>
        <p>将从 Codex 中移除 <strong>{pendingUninstall.record.manifest.name}</strong>，本地版本会保留。</p>
        {pendingUninstall.modified_files.length ? <div className="skill-dialog-warning"><CircleAlert size={16} />检测到用户修改。请先使用“修复并备份”保留当前文件。</div> : null}
        <div className="skill-dialog-actions"><button className="btn" onClick={() => setPendingUninstall(null)}>取消</button><button className="btn btn-danger" disabled={isBusy || pendingUninstall.modified_files.length > 0} onClick={() => { onUninstall(pendingUninstall.record.manifest.id); setPendingUninstall(null); }}><Trash2 size={15} />确认卸载</button></div>
      </div></div> : null}
	  {draftEditorOpen ? <DraftEditor draft={editingDraft} authorName={authorName} availablePlugins={availablePlugins} onClose={() => setDraftEditorOpen(false)} onSave={async input => { await onSaveDraft(input); setDraftEditorOpen(false); }} /> : null}
	  {installPlan || planError ? <InstallPlanDialog plan={installPlan} error={planError} currentVersion={installPlan ? installedById.get(installPlan.skill.skill_id)?.record.manifest.version : undefined} busy={isBusy} onClose={() => { setInstallPlan(null); setPlanError(''); }} onInstall={(optionalIds) => { if (installPlan) onInstallMarketplace(installPlan.skill.skill_id, installPlan.skill.version, optionalIds); setInstallPlan(null); }} /> : null}
    </div>
  );
}

function MarketSkillListItem({ item, installed, selected, onSelect }: { item: OrganizationSkillCatalogItem; installed?: CodexSkillStatusItem; selected: boolean; onSelect: (id: string) => void }) {
  const update = installed ? compareSemanticVersions(item.version, installed.record.manifest.version) > 0 : false;
  const managed = item.assignment === 'required' && item.management !== 'user_managed';
  const label = item.assignment === 'blocked' ? '不可安装' : update ? '可更新' : installed ? '已安装' : managed ? '由组织管理' : item.assignment === 'recommended' ? '组织推荐' : '可安装';
  const tone = item.assignment === 'blocked' ? 'danger' : update ? 'warn' : installed ? 'success' : managed ? 'warn' : 'neutral';
  return <button className={`skill-browser-item ${selected ? 'selected' : ''}`} onClick={() => onSelect(item.skill_id)}><span className={`skill-state-rail ${tone}`} /><span className="skill-browser-item-copy"><strong>{item.name}</strong><small>{item.source === 'organization' ? '组织提供' : '公共技能库'} · {item.description || '暂无用途说明'}</small><small className="catalog-item-author">作者：{item.author_name || '马宝全'}</small></span><span className={`skill-state-label ${tone}`}>{label}</span></button>;
}

const SKILL_CLIENTS = [
  { id: 'codex', name: 'Codex', mark: 'C' },
  { id: 'github-copilot', name: 'GitHub Copilot', mark: 'G' },
  { id: 'workbuddy', name: 'WorkBuddy', mark: 'W' },
] as const;

function SkillClientSummary({ status }: { status: CodexSkillStatusResponse | null }) {
  return <section className="skill-client-summary" aria-label="AI 工具技能状态">
    <div className="skill-client-summary-intro"><span className="skill-client-mark">AI</span><div><small>AI 工具</small><strong>技能将安装到可用工具</strong></div></div>
    {SKILL_CLIENTS.map(client => {
      const clientStatus = statusForClient(status, client.id);
      const installedCount = clientStatus?.items.filter(item => item.client_state !== 'not_installed').length || 0;
      const attentionCount = clientStatus?.items.filter(item => ['outdated', 'modified', 'failed'].includes(item.client_state)).length || 0;
      return <div className="skill-client-card" key={client.id}>
        <span className="skill-client-mark">{client.mark}</span>
        <div><small>{client.name}</small><strong><span className={`status-dot ${clientStatusTone(clientStatus)}`} /> {clientStatusLabel(clientStatus)}</strong><span className="skill-client-count">{clientStatus ? `${installedCount} 个技能${attentionCount ? ` · ${attentionCount} 个待处理` : ''}` : '等待检测'}</span></div>
      </div>;
    })}
  </section>;
}

function statusForClient(status: CodexSkillStatusResponse | null, clientId: string) {
  if (!status) return undefined;
  return clientId === 'codex' ? status : status.clients?.[clientId];
}

function clientStatusLabel(status?: CodexSkillStatusResponse) {
  if (!status) return '未检测';
  if (status.target_mode === 'preview') return '未连接';
  if (status.target_configured || status.target_exists || status.target_mode === 'detected' || status.target_mode === 'configured') return '已就绪';
  return '未连接';
}

function clientStatusTone(status?: CodexSkillStatusResponse) {
  if (!status || status.target_mode === 'preview') return 'neutral';
  return status.target_configured || status.target_exists || status.target_mode === 'detected' || status.target_mode === 'configured' ? 'success' : 'warn';
}

function MarketSkillDetail({ item, installed, availablePlugins, busyAction, onLoadVersions, onPlan, planLoading }: { item: OrganizationSkillCatalogItem; installed?: CodexSkillStatusItem; availablePlugins: PluginCatalogItem[]; busyAction: string | null; onLoadVersions: (skillId: string) => Promise<OrganizationSkillCatalogItem[]>; onPlan: (version?: string) => void; planLoading: boolean }) {
  const update = installed ? compareSemanticVersions(item.version, installed.record.manifest.version) > 0 : false;
  const current = Boolean(installed) && !update;
  const busy = busyAction === `market:${item.skill_id}`;
	const managed = item.assignment === 'required' && item.management !== 'user_managed';
	const lockedLabel = item.assignment === 'blocked' ? '不可安装' : managed ? '由组织管理' : undefined;
  const [tab, setTab] = useState<'details' | 'versions'>('details');
  const [versions, setVersions] = useState<OrganizationSkillCatalogItem[]>([item]);
  const [versionsLoading, setVersionsLoading] = useState(false);
  const [versionsError, setVersionsError] = useState('');

  useEffect(() => {
    if (tab !== 'versions') return;
    let active = true;
    setVersionsLoading(true);
    setVersionsError('');
    onLoadVersions(item.skill_id).then(result => { if (active) setVersions(result.length ? result : [item]); }).catch(() => { if (active) setVersionsError('暂时无法读取版本，请稍后重试。'); }).finally(() => { if (active) setVersionsLoading(false); });
    return () => { active = false; };
  }, [item, onLoadVersions, tab]);

  return <>
    <header className="skill-detail-header"><div className="skill-detail-title"><span className="skill-detail-mark">{item.name.slice(0, 1).toUpperCase()}</span><div><div className="skill-title-line"><h3>{item.name}</h3><Pill kind={lockedLabel ? 'warn' : 'success'}><BadgeCheck size={12} />{lockedLabel || '已审核'}</Pill></div><small className="skill-detail-source">{item.source === 'organization' ? '组织提供' : '公共技能库'} · {item.author_name || '未知作者'}</small></div></div><div className="skill-detail-actions"><button className="btn btn-primary" disabled={busy || planLoading || current || Boolean(lockedLabel)} onClick={() => onPlan(item.version)}><Download className={busy || planLoading ? 'spin' : ''} size={15} />{lockedLabel || (busy ? '正在安装' : planLoading ? '正在检查' : update ? '更新' : current ? '已安装' : '安装')}</button></div></header>
	{item.organization_reason ? <div className="skill-detail-notice modified"><Building2 size={16} /><div><strong>组织说明</strong><span>{item.organization_reason}</span></div></div> : null}
    <SkillDetailTabs value={tab} onChange={setTab} />
    {tab === 'versions' ? <SkillVersionList versions={versions} currentVersion={installed?.record.manifest.version} lockedLabel={lockedLabel} loading={versionsLoading} error={versionsError} onSelect={onPlan} /> : <>
      <section className="skill-detail-section"><div className="skill-section-title"><div><strong>功能</strong></div></div><p className="skill-release-notes">{item.description || '暂无功能说明。'}</p><div className="market-taxonomy"><Tags items={functionalCategoryLabels(item.categories)} /></div></section>
      <section className="skill-detail-section"><div className="skill-section-title"><div><strong>依赖</strong></div></div><div className="skill-dependency-list">{(item.plugin_dependencies || []).map(dependency => <div key={dependency.plugin_id}><span className="status-dot success" /><PluginDependencyIdentity pluginId={dependency.plugin_id} plugins={availablePlugins} /><span>{dependency.required ? '必需' : '可选'}</span><strong>{dependency.min_version ? `v${dependency.min_version} 及以上` : '不限版本'}</strong></div>)}{!item.plugin_dependencies?.length ? <span className="skill-section-empty">无依赖</span> : null}</div></section>
      <details className="plugin-technical-panel"><summary>开发者信息</summary><div className="skill-release-grid"><div><small>Skill ID</small><code>{item.skill_id}</code></div><div><small>最低 Agent 版本</small><strong>{item.min_agent_version ? `v${item.min_agent_version}` : '--'}</strong></div><div><small>支持的 AI 客户端</small><strong>{item.supported_clients.join('、') || '--'}</strong></div></div></details>
    </>}
  </>;
}

function SkillDetailTabs({ value, onChange }: { value: 'details' | 'versions'; onChange: (value: 'details' | 'versions') => void }) {
  return <div className="extension-detail-tabs" role="tablist"><button role="tab" aria-selected={value === 'details'} className={value === 'details' ? 'active' : ''} onClick={() => onChange('details')}>详情</button><button role="tab" aria-selected={value === 'versions'} className={value === 'versions' ? 'active' : ''} onClick={() => onChange('versions')}>版本</button></div>;
}

type SkillVersionDisplay = Pick<OrganizationSkillCatalogItem, 'version' | 'release_notes' | 'published_at'>;

function SkillVersionList({ versions, currentVersion, lockedLabel, loading, error, onSelect }: { versions: SkillVersionDisplay[]; currentVersion?: string; lockedLabel?: string; loading: boolean; error: string; onSelect?: (version: string) => void }) {
  const sorted = [...versions].sort((left, right) => compareSemanticVersions(right.version, left.version));
  return <section className="extension-version-list">{loading ? <div className="extension-version-empty"><span className="spinner" />正在读取版本</div> : null}{error ? <div className="skill-inline-warning"><CircleAlert size={15} /><span>{error}</span></div> : null}{!loading && sorted.map(version => { const installed = currentVersion === version.version; const newer = currentVersion ? compareSemanticVersions(version.version, currentVersion) > 0 : false; const action = installed ? '已安装' : lockedLabel || (currentVersion ? newer ? '更新' : '切换' : '安装'); return <article className="extension-version-row" key={version.version}><div className="extension-version-main"><div><strong>v{version.version}</strong>{installed ? <Pill kind="success">已安装</Pill> : null}</div><time>{formatPublishedAt(version.published_at)}</time><p>{version.release_notes || '未提供更新说明。'}</p></div>{onSelect ? <button className={installed || lockedLabel ? 'btn' : 'btn btn-primary'} disabled={installed || Boolean(lockedLabel)} onClick={() => onSelect(version.version)}>{action}</button> : null}</article>; })}</section>;
}

function RemoteSkillSubmissionListItem({ item, selected, onSelect }: { item: SkillSubmissionStatus; selected: boolean; onSelect: (id: string) => void }) {
  const state = skillSubmissionState(item);
  return <button className={`skill-browser-item ${selected ? 'selected' : ''}`} onClick={() => onSelect(`remote:${item.id}`)}><span className={`skill-state-rail ${state.tone}`} /><span className="skill-browser-item-copy"><strong>{item.name || item.product_key}</strong><small>{skillCreationRoleLabel(item.role)}</small><small>{item.product_key} · v{item.version} · 未关联本地源码</small></span><span className={`skill-state-label ${state.tone}`}>{state.label}</span></button>;
}

function RemoteSkillSubmissionDetail({ item, onImportCandidate }: { item: SkillSubmissionStatus; onImportCandidate: (revisionOfVersion?: string, parentSubmissionId?: string) => void }) {
  const state = skillSubmissionState(item);
  const canMaintain = item.role === 'owner' || item.role === 'contributor';
  return <>
    <header className="skill-detail-header"><div className="skill-detail-title"><span className="skill-detail-mark">{(item.name || item.product_key).slice(0, 1).toUpperCase()}</span><div><div className="skill-title-line"><h3>{item.name || item.product_key}</h3><Pill kind={state.tone === 'neutral' ? 'warn' : state.tone}>{state.label}</Pill><Pill kind="warn">未关联本地源码</Pill></div><small className="skill-detail-source">{skillCreationRoleLabel(item.role)}{item.author_name ? ` · 作者：${item.author_name}` : ''}</small><code>{item.product_key} · v{item.version}</code></div></div><div className="skill-detail-actions">{canMaintain ? <button className="btn btn-primary" onClick={() => onImportCandidate(item.version, item.id)}><PackagePlus size={15} />关联新版本候选</button> : null}</div></header>
    {item.review_note ? <div className="skill-detail-notice"><CircleAlert size={16} /><div><strong>审核意见</strong><span>{normalizeReviewNote(item.review_note)}</span></div></div> : null}
    <div className="skill-detail-meta"><div><span>版本</span><strong>v{item.version}</strong></div><div><span>版本来源</span><strong>{item.revision_of_version ? `基于 v${item.revision_of_version}` : '首个版本'}</strong></div><div><span>审核状态</span><strong>{state.label}</strong></div><div><span>发布状态</span><strong>{skillReleaseStatusLabel(item.release_status)}</strong></div></div>
    <section className="skill-detail-section"><div className="skill-section-title"><div><Files size={16} /><strong>版本更新说明</strong></div><span>v{item.version}</span></div><p className="skill-release-notes">{item.release_notes || '该提交未提供更新说明。'}</p></section>
    <section className="skill-detail-section"><div className="skill-section-title"><div><BookOpen size={16} /><strong>提交记录</strong></div><span>{skillSourceTypeLabel(item.source_type)}</span></div><div className="skill-submission-id"><span>{skillCreationRoleLabel(item.role)}</span><code>{item.id}</code></div></section>
  </>;
}

function DraftListItem({ item, submission, selected, onSelect }: { item: AuthoringSkillDraft; submission?: SkillSubmissionStatus; selected: boolean; onSelect: (id: string) => void }) {
  const state = draftState(item, submission);
  return <button className={`skill-browser-item ${selected ? 'selected' : ''}`} onClick={() => onSelect(draftKey(item))}><span className={`skill-state-rail ${state.tone}`} /><span className="skill-browser-item-copy"><strong>{item.manifest.name}</strong><small>作者：{item.manifest.author || '马宝全'}</small><small>{item.manifest.id} · v{item.manifest.version}</small></span><span className={`skill-state-label ${state.tone}`}>{state.label}</span></button>;
}

function DraftDetail({ item, availablePlugins, submission, busyAction, onCreateRevision, onEdit, onTest, onConfirm, onSubmit, onOpenDirectory }: { item: AuthoringSkillDraft; availablePlugins: PluginCatalogItem[]; submission?: SkillSubmissionStatus; busyAction: string | null; onCreateRevision: (id: string, version: string) => void; onEdit: () => void; onTest: (id: string, version: string) => void; onConfirm: (id: string, version: string) => void; onSubmit: (id: string, version: string) => void; onOpenDirectory: (path: string) => void }) {
  const manifest = item.manifest;
  const state = draftState(item, submission);
  const busy = Boolean(busyAction?.endsWith(manifest.id));
  const canRevise = !item.submitted_at;
  return <>
    <header className="skill-detail-header"><div className="skill-detail-title"><span className="skill-detail-mark">{manifest.name.slice(0, 1).toUpperCase()}</span><div><div className="skill-title-line"><h3>{manifest.name}</h3><Pill kind={state.tone === 'success' ? 'success' : state.tone === 'warn' ? 'warn' : 'danger'}>{state.label}</Pill></div><small className="skill-detail-source">作者：{manifest.author || '马宝全'}</small><code>{manifest.id} · v{manifest.version}</code></div></div><div className="skill-detail-actions">
      {canRevise ? <button className="btn" disabled={busy} onClick={onEdit}><Edit3 size={15} />编辑</button> : null}
      {!canRevise ? <button className="btn btn-primary" disabled={busy} onClick={() => onCreateRevision(manifest.id, manifest.version)}><Plus size={15} />创建新版本</button> : null}
      {canRevise ? <button className="btn btn-primary" disabled={busy} onClick={() => onTest(manifest.id, manifest.version)}><Play className={busyAction === `test:${manifest.id}` ? 'spin' : ''} size={15} />部署测试</button> : null}
      {item.tested_at && !item.confirmed_at && canRevise ? <button className="btn" disabled={busy} onClick={() => onConfirm(manifest.id, manifest.version)}><CheckCircle2 size={15} />确认通过</button> : null}
      {item.confirmed_at && canRevise ? <button className="btn btn-primary" disabled={busy} onClick={() => onSubmit(manifest.id, manifest.version)}><Send size={15} />提交审核</button> : null}
    </div></header>
    <p className="skill-detail-description">{manifest.description || '暂无用途说明。'}</p>
	{submission?.review_note ? <div className={`skill-detail-notice ${submission.status === 'approved' ? 'modified' : ''}`}><CircleAlert size={16} /><div><strong>审核意见</strong><span>{normalizeReviewNote(submission.review_note)}</span></div></div> : null}
    <div className="skill-authoring-pipeline"><DraftStage complete label="内容已保存" /><DraftStage complete={Boolean(item.tested_at)} label="AI 客户端已部署" /><DraftStage complete={Boolean(item.confirmed_at)} label="测试已确认" /><DraftStage complete={Boolean(item.submitted_at)} label="已提交审核" /></div>
    <div className="skill-detail-meta"><div><span>候选版本</span><strong>v{manifest.version}</strong></div><div><span>版本来源</span><strong>{item.revision_of ? `基于 v${item.revision_of}` : '新建工作区'}</strong></div><div><span>版本状态</span><strong>{item.submitted_at ? '已冻结' : '可编辑'}</strong></div><div><span>测试时间</span><strong>{formatAuthoringTime(item.tested_at)}</strong></div><div><span>客户端目标</span><strong>{Object.keys(item.client_targets || {}).length || manifest.supported_clients?.length || 0}</strong></div><div><span>插件依赖</span><strong>{manifest.plugin_dependencies?.length || 0}</strong></div><div><span>风险</span><strong>{riskLabel(manifest.risk_summary)}</strong></div></div>
    <section className="skill-detail-section"><div className="skill-section-title"><div><Files size={16} /><strong>版本更新说明</strong></div><span>v{manifest.version}</span></div><p className="skill-release-notes">{manifest.release_notes || '保存或测试前需要补充本版本更新说明。'}</p></section>
    <section className="skill-detail-section"><div className="skill-section-title"><div><ShieldCheck size={16} /><strong>所需插件</strong></div><span>{manifest.plugin_dependencies?.length || 0}</span></div><div className="skill-dependency-list">{(manifest.plugin_dependencies || []).map(dependency => <div key={dependency.plugin_id}><span className="status-dot success" /><PluginDependencyIdentity pluginId={dependency.plugin_id} plugins={availablePlugins} /><span>{dependency.required ? '必需' : '可选'}</span><strong>{dependency.min_version ? `>= v${dependency.min_version}` : '任意版本'}</strong></div>)}{!manifest.plugin_dependencies?.length ? <span className="skill-section-empty">无需关联插件</span> : null}</div></section>
    <section className="skill-detail-section"><div className="skill-section-title"><div><BookOpen size={16} /><strong>候选制品</strong></div><span>SHA-256</span></div><div className="skill-artifact-local"><code title={item.candidate_sha256}>{item.candidate_sha256}</code><button className="text-action" onClick={() => onOpenDirectory(item.candidate_path)}><FolderOpen size={14} />打开位置</button></div>{item.dashboard_draft_id ? <div className="skill-submission-id"><span>审核记录</span><code>{item.dashboard_draft_id}</code></div> : null}</section>
    <section className="skill-detail-section"><div className="skill-section-title"><div><BookOpen size={16} /><strong>SKILL.md</strong></div><span>{item.readme.length} 字符</span></div><pre className="skill-draft-readme">{item.readme}</pre></section>
  </>;
}

function DraftStage({ complete, label }: { complete: boolean; label: string }) { return <div className={complete ? 'complete' : ''}><span>{complete ? <CheckCircle2 size={13} /> : null}</span><strong>{label}</strong></div>; }

function DraftEditor({ draft, authorName, availablePlugins, onClose, onSave }: { draft: AuthoringSkillDraft | null; authorName: string; availablePlugins: PluginCatalogItem[]; onClose: () => void; onSave: (input: AuthoringSkillDraftInput) => Promise<void> }) {
  const [input, setInput] = useState<AuthoringSkillDraftInput>(() => draftInputFrom(draft, authorName));
  const [capabilityText, setCapabilityText] = useState(() => (draft?.manifest.capabilities || []).map(item => `${item.id}|${item.min_version || ''}|${item.required ? 'required' : 'optional'}|${item.provider || ''}`).join('\n'));
  const [pluginText, setPluginText] = useState(() => (draft?.manifest.plugin_dependencies || []).map(item => `${item.plugin_id}|${item.min_version || ''}|${item.required ? 'required' : 'optional'}`).join('\n'));
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState('');
	const selectedPluginIds = new Set(parsePluginDependencies(pluginText).map(item => item.plugin_id));
	function togglePlugin(plugin: PluginCatalogItem, checked: boolean) {
		const dependencies = parsePluginDependencies(pluginText).filter(item => item.plugin_id !== plugin.plugin_id);
		if (checked) dependencies.push({ plugin_id: plugin.plugin_id, min_version: plugin.version, required: true });
		setPluginText(dependencies.map(item => `${item.plugin_id}|${item.min_version || ''}|${item.required ? 'required' : 'optional'}`).join('\n'));
	}
  const change = (field: keyof AuthoringSkillDraftInput, value: string) => setInput(current => ({ ...current, [field]: value }));
  const changeCategory = (value: string) => setInput(current => ({ ...current, categories: value ? [value] : [] }));
  async function save() {
    setSaving(true); setSaveError('');
    try { await onSave({ ...input, capabilities: parseCapabilities(capabilityText), plugin_dependencies: parsePluginDependencies(pluginText) }); }
    catch (reason) { setSaveError(reason instanceof Error ? reason.message : String(reason)); }
    finally { setSaving(false); }
  }
  return <div className="skill-dialog-backdrop"><div className="skill-dialog skill-editor-dialog" role="dialog" aria-modal="true"><div className="skill-dialog-head"><strong>{draft ? '编辑 Skill' : '新建 Skill'}</strong><button className="btn btn-icon" onClick={onClose} aria-label="关闭"><X size={16} /></button></div>{saveError ? <div className="skill-dialog-warning"><CircleAlert size={16} />{saveError}</div> : null}<div className="skill-editor-grid">
    <label><span>名称</span><input value={input.name} placeholder="例如：项目交付检查" onChange={event => change('name', event.target.value)} /></label><label><span>作者</span><input value={input.author} disabled /></label><label><span>主功能分类</span><select value={functionalCategoryID(input.categories[0])} onChange={event => changeCategory(event.target.value)}><option value="">请选择分类</option>{FUNCTIONAL_CATEGORIES.map(category => <option key={category.id} value={category.id}>{category.label}</option>)}</select></label><label className="wide"><span>用途说明</span><textarea rows={2} value={input.description} placeholder="说明这个 Skill 适合在什么情况下使用" onChange={event => change('description', event.target.value)} /></label><label className="wide"><span>版本更新说明</span><textarea rows={3} value={input.release_notes} placeholder="说明本版本新增、改进或修复的内容" onChange={event => change('release_notes', event.target.value)} /></label><label className="wide"><span>Skill 指令</span><textarea className="skill-editor-readme" rows={15} value={input.readme} onChange={event => change('readme', event.target.value)} /></label>
    <section className="wide skill-editor-plugin-picker"><span>需要使用的工具插件</span><div>{availablePlugins.filter(plugin => plugin.governance !== 'blocked').map(plugin => <label key={plugin.plugin_id}><input type="checkbox" checked={selectedPluginIds.has(plugin.plugin_id)} onChange={event => togglePlugin(plugin, event.target.checked)} /><span><strong>{plugin.name}</strong><small>{plugin.description || '未提供用途说明'} · v{plugin.version}</small></span></label>)}{!availablePlugins.length ? <small>插件库暂无可选工具，仍可先保存技能内容。</small> : null}</div></section>
    <details className="wide skill-editor-advanced"><summary>开发者设置</summary><div className="skill-editor-grid"><label><span>Skill ID</span><input value={input.id} disabled={Boolean(draft)} onChange={event => change('id', event.target.value)} /></label><label><span>版本</span><input value={input.version} disabled={Boolean(draft)} onChange={event => change('version', event.target.value)} /></label><label><span>最低 Agent</span><input value={input.min_agent_version} onChange={event => change('min_agent_version', event.target.value)} /></label><label><span>操作范围</span><select value={input.risk_summary} onChange={event => change('risk_summary', event.target.value)}><option value="read_only">只读取信息</option><option value="local_action">修改本机内容</option><option value="network_write">修改网络内容</option><option value="approval_required">每次操作前确认</option></select></label><label className="wide"><span>高级能力依赖</span><textarea rows={4} value={capabilityText} onChange={event => setCapabilityText(event.target.value)} placeholder="system.health|1.0.0|required|agent" /></label></div></details>
  </div><div className="skill-dialog-actions"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-primary" disabled={saving || !input.name.trim() || !input.release_notes.trim() || !input.readme.trim()} onClick={save}>{saving ? '正在保存' : '保存草稿'}</button></div></div></div>;
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

function SkillDetail({ item, availablePlugins, catalogPolicy, busyAction, onLoadVersions, onPlanVersion, onSync, onRepair, onUninstall, onOpenDirectory }: { item: CodexSkillStatusItem; availablePlugins: PluginCatalogItem[]; catalogPolicy?: OrganizationSkillCatalogItem; busyAction: string | null; onLoadVersions: (skillId: string) => Promise<OrganizationSkillCatalogItem[]>; onPlanVersion: (version: string) => void; onSync: (id: string) => void; onRepair: (id: string) => void; onUninstall: () => void; onOpenDirectory: (path: string) => void }) {
  const manifest = item.record.manifest;
  const actionBusy = Boolean(busyAction?.endsWith(manifest.id));
  const [tab, setTab] = useState<'details' | 'versions'>('details');
  const [versions, setVersions] = useState<SkillVersionDisplay[]>(catalogPolicy ? [catalogPolicy] : [{ version: manifest.version, release_notes: manifest.release_notes || '' }]);
  const [versionsLoading, setVersionsLoading] = useState(false);
  const [versionsError, setVersionsError] = useState('');
  const managed = catalogPolicy?.management !== 'user_managed' && Boolean(catalogPolicy?.managed);

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
      {item.available_actions.includes('install') || item.available_actions.includes('update') ? <button className="btn btn-primary" disabled={actionBusy} onClick={() => onSync(manifest.id)}><RefreshCw className={actionBusy ? 'spin' : ''} size={15} />{item.client_state === 'outdated' ? '更新' : '安装'}</button> : null}
      {item.available_actions.includes('repair') ? <button className="btn" disabled={actionBusy} onClick={() => onRepair(manifest.id)}><Wrench size={15} />{item.client_state === 'modified' ? '修复并备份' : '重新同步'}</button> : null}
      {item.available_actions.includes('uninstall') && catalogPolicy?.allow_uninstall !== false ? <button className="btn btn-danger-quiet" disabled={actionBusy} onClick={onUninstall}><Trash2 size={15} />卸载</button> : null}
    </div></header>
    <SkillDetailTabs value={tab} onChange={setTab} />
    {item.readiness.state !== 'ready' ? <div className="skill-detail-notice"><CircleAlert size={16} /><div><strong>{item.readiness.state === 'blocked' ? '当前不可安装' : '部分功能不可用'}</strong><span>请安装所需插件或更新 HiMind Agent。</span></div></div> : null}
    {item.client_state === 'modified' ? <div className="skill-detail-notice modified"><Wrench size={16} /><div><strong>检测到技能文件已被修改</strong><span>修复前会自动保留一份备份。</span></div></div> : null}
    {tab === 'versions' ? <SkillVersionList versions={versions} currentVersion={item.installed_version || manifest.version} lockedLabel={catalogPolicy?.assignment === 'blocked' ? '不可安装' : managed ? '由组织管理' : undefined} loading={versionsLoading} error={versionsError} onSelect={catalogPolicy ? onPlanVersion : undefined} /> : <>
      <section className="skill-detail-section"><div className="skill-section-title"><div><strong>功能</strong></div></div><p className="skill-release-notes">{manifest.description || '暂无功能说明。'}</p></section>
      <section className="skill-detail-section"><div className="skill-section-title"><div><strong>依赖</strong></div></div><div className="skill-dependency-list">{(manifest.plugin_dependencies || []).map(dependency => <div key={dependency.plugin_id}><span className="status-dot success" /><PluginDependencyIdentity pluginId={dependency.plugin_id} plugins={availablePlugins} /><span>{dependency.required ? '必需' : '可选'}</span><strong>{dependency.min_version ? `v${dependency.min_version} 及以上` : '不限版本'}</strong></div>)}{!manifest.plugin_dependencies?.length ? <span className="skill-section-empty">无依赖</span> : null}</div></section>
      <details className="plugin-technical-panel"><summary>开发者信息</summary><div className="plugin-technical-grid"><div><span>Skill ID</span><code>{manifest.id}</code></div><div><span>作者</span><strong>{manifest.author || '未知作者'}</strong></div><div><span>来源</span><strong>{scopeLabel(manifest.scope)}</strong></div><div><span>最近同步</span><strong>{formatSyncedAt(item.last_synced_at)}</strong></div><div className="wide"><span>本地目录</span><code>{item.rendered_root || '--'}</code></div></div><div className="skill-file-summary"><span /><button className="text-action" disabled={!item.rendered} onClick={() => onOpenDirectory(item.rendered_root)}><FolderOpen size={14} />打开目录</button></div></details>
    </>}
  </>;
}

function clientStateLabel(state: CodexSkillStatusItem['client_state']) {
  const labels: Record<CodexSkillStatusItem['client_state'], string> = { not_installed: '未安装', installed: '已安装', outdated: '有更新', modified: '已修改', blocked: '不可用', unsupported: '不兼容', failed: '失败' };
  return labels[state] || state;
}

function stateTone(state: CodexSkillStatusItem['client_state']) { if (state === 'installed') return 'success'; if (state === 'outdated' || state === 'modified') return 'warn'; if (state === 'not_installed') return 'neutral'; return 'danger'; }
function statePill(state: CodexSkillStatusItem['client_state']): 'success' | 'warn' | 'danger' { if (state === 'installed') return 'success'; if (state === 'outdated' || state === 'modified' || state === 'not_installed') return 'warn'; return 'danger'; }
function scopeLabel(scope: string) { if (scope === 'builtin') return '系统内置'; if (scope === 'organization') return '技能市场'; if (scope === 'user') return '我的技能'; return scope || '--'; }
function riskLabel(value?: string) { return ({ read_only: '只读', local_action: '本地操作', network_write: '网络写入', approval_required: '需要审批' } as Record<string, string>)[value || ''] || (value ? '未分类风险' : '未声明'); }
function normalizeReviewNote(value: string) { return /\?{3,}|�/.test(value) ? '审核意见数据无法正常解码，请重新提交中文审核意见。' : value; }
function formatSyncedAt(value?: string | null) { if (!value) return '尚未同步'; const milliseconds = Number.parseInt(value.split('-')[0], 10); return Number.isFinite(milliseconds) ? new Date(milliseconds).toLocaleString('zh-CN', { hour12: false }) : value; }
function formatPublishedAt(value?: string) { if (!value) return '发布时间未知'; const date = new Date(value); return Number.isNaN(date.getTime()) ? value : date.toLocaleDateString('zh-CN'); }
function draftKey(draft?: AuthoringSkillDraft) { return draft ? `${draft.manifest.id}@${draft.manifest.version}` : ''; }
function draftState(draft: AuthoringSkillDraft, submission?: SkillSubmissionStatus): { label: string; tone: 'success' | 'warn' | 'neutral' | 'danger' } { if (submission?.release_status === 'revoked') return { label: '已撤回', tone: 'danger' }; if (submission?.status === 'approved' && submission.release_status === 'published') return { label: '已上架', tone: 'success' }; if (submission?.status === 'approved') return { label: '审核通过，待发布', tone: 'warn' }; if (submission?.status === 'changes_requested') return { label: '需修改', tone: 'warn' }; if (submission?.status === 'rejected') return { label: '已拒绝', tone: 'danger' }; if (draft.submitted_at) return { label: '审核中', tone: 'success' }; if (draft.confirmed_at) return { label: '可提交', tone: 'success' }; if (draft.tested_at) return { label: '待确认', tone: 'warn' }; return { label: '草稿', tone: 'neutral' }; }
function skillSubmissionState(submission: SkillSubmissionStatus): { label: string; tone: 'success' | 'warn' | 'neutral' | 'danger' } { if (submission.release_status === 'revoked') return { label: '已撤回', tone: 'danger' }; if (submission.status === 'approved' && submission.release_status === 'published') return { label: '已上架', tone: 'success' }; if (submission.status === 'approved') return { label: '审核通过，待发布', tone: 'warn' }; if (submission.status === 'changes_requested') return { label: '需修改', tone: 'warn' }; if (submission.status === 'rejected') return { label: '已拒绝', tone: 'danger' }; return { label: '审核中', tone: 'warn' }; }
function skillCreationRoleLabel(role?: SkillSubmissionStatus['role']) { if (role === 'owner') return '我是作者'; if (role === 'contributor') return '我是贡献者'; return '我的提交'; }
function skillReleaseStatusLabel(status?: string) { if (status === 'published') return '已上架'; if (status === 'draft') return '待发布'; if (status === 'revoked') return '已撤回'; return '未发布'; }
function skillSourceTypeLabel(source?: string) { if (source === 'repository_snapshot') return '源码快照'; if (source === 'archive') return '归档包'; return '本地源码'; }
function formatAuthoringTime(value?: string | null) { if (!value) return '--'; const time = Number.parseInt(value, 10); return Number.isFinite(time) ? new Date(time).toLocaleString('zh-CN', { hour12: false }) : value; }
function draftInputFrom(draft: AuthoringSkillDraft | null, authorName: string): AuthoringSkillDraftInput { return draft ? { id: draft.manifest.id, name: draft.manifest.name, author: authorName || draft.manifest.author || '当前授权用户', categories: draft.manifest.categories || [], version: draft.manifest.version, description: draft.manifest.description || '', release_notes: draft.manifest.release_notes || '', min_agent_version: draft.manifest.min_agent_version || '0.3.1', supported_clients: draft.manifest.supported_clients || ['codex', 'github-copilot', 'workbuddy'], capabilities: draft.manifest.capabilities || [], plugin_dependencies: draft.manifest.plugin_dependencies || [], risk_summary: draft.manifest.risk_summary || 'read_only', readme: draft.readme, files: draft.files || {} } : { id: `com.himind.skill.custom-${Date.now().toString(36)}`, name: '', author: authorName || '请先授权工作台账号', categories: ['software-engineering'], version: '0.1.0', description: '', release_notes: '', min_agent_version: '0.3.1', supported_clients: ['codex', 'github-copilot', 'workbuddy'], capabilities: [{ id: 'system.health', required: true, min_version: '1.0.0', provider: 'agent' }], plugin_dependencies: [], risk_summary: 'read_only', readme: '# 技能名称\n\n## 需要的信息\n\n列出执行前需要向用户确认的信息。\n\n## 执行步骤\n\n1. 检查必要输入与依赖。\n2. 只调用已声明的 Capability。\n3. 汇总结果和需要用户处理的问题。\n\n## 输出要求\n\n说明希望 AI 返回的内容和格式。\n\n## 禁止事项\n\n- 不绕过 Capability Gateway。\n- 不展示账号、Cookie、Token 或其他凭据。\n', files: {} }; }
function parseCapabilities(value: string): AuthoringSkillDraftInput['capabilities'] { return value.split(/\r?\n/).map(line => line.trim()).filter(Boolean).map(line => { const [id, minVersion, mode, provider] = line.split('|').map(part => part.trim()); return { id, min_version: minVersion || undefined, required: mode !== 'optional', provider: provider || undefined }; }); }
function parsePluginDependencies(value: string): AuthoringSkillDraftInput['plugin_dependencies'] { return value.split(/\r?\n/).map(line => line.trim()).filter(Boolean).map(line => { const [pluginId, minVersion, mode] = line.split('|').map(part => part.trim()); return { plugin_id: pluginId, min_version: minVersion || undefined, required: mode !== 'optional' }; }); }
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
