import { useEffect, useMemo, useState } from 'react';
import {
  BookOpen,
  ArrowLeft,
  BadgeCheck,
  Building2,
  CircleAlert,
  FolderOpen,
  Download,
  RefreshCw,
  Files,
  Link2,
  Search,
  Sparkles,
  Trash2,
  Wrench,
  X,
} from 'lucide-react';
import { EmptyState, PageHeader, Pill, Tags } from '../components/Common';
import type { CatalogPage, CodexSkillStatusItem, CodexSkillStatusResponse, ExtensionDesiredItem, ExtensionDesiredState, OrganizationSkillCatalogItem, PluginCatalogItem, PluginRegistry, SkillCatalogResponse, SkillInstallPlan, SkillSyncSettings } from '../services/agentApi';
import { FUNCTIONAL_CATEGORIES, categorySearchText, functionalCategoryLabels, functionalCategoryMatches } from '../data/categoryCatalog';
import { ManagedCapabilitiesPanel } from './ManagedCapabilitiesPage';

type SkillsWorkspacePageProps = {
  catalog: SkillCatalogResponse | null;
  status: CodexSkillStatusResponse | null;
  error: string | null;
  marketplace: OrganizationSkillCatalogItem[];
  marketplaceError: string | null;
	dashboardEnabled: boolean;
	desired: ExtensionDesiredState | null;
	desiredLoading: boolean;
	desiredError: string | null;
	pluginRegistry: PluginRegistry | null;
	marketEnabled: boolean;
	onQueryMarketplace: (q: string, category: string, page?: number, pageSize?: number) => Promise<CatalogPage<OrganizationSkillCatalogItem>>;
	availablePlugins: PluginCatalogItem[];
  busyAction: string | null;
  onRefresh: () => void;
  onSyncAll: () => void;
  onSyncSkill: (skillId: string) => void;
  syncMode: SkillSyncSettings['mode'];
  onSetSyncMode: (mode: SkillSyncSettings['mode']) => void;
  onLoadVersions: (skillId: string) => Promise<OrganizationSkillCatalogItem[]>;
  onPlanMarketplace: (skillId: string, version?: string) => Promise<SkillInstallPlan>;
  onInstallMarketplace: (skillId: string, version: string | undefined, optionalPluginIds: string[]) => void;
  onRepair: (skillId: string) => void;
  onUninstall: (skillId: string) => void;
  onOpenDirectory: (path: string) => void;
  onImportLocal: () => void;
  onImportGithub: (sourceUrl: string) => Promise<void>;
};

type ViewKey = 'marketplace' | 'installed' | 'system';

export function SkillsWorkspacePage({ catalog, status, error, marketplace, marketplaceError, desired, desiredLoading, desiredError, pluginRegistry, dashboardEnabled, marketEnabled, onQueryMarketplace, availablePlugins, busyAction, syncMode, onSetSyncMode, onRefresh, onSyncAll, onSyncSkill, onLoadVersions, onPlanMarketplace, onInstallMarketplace, onRepair, onUninstall, onOpenDirectory, onImportLocal, onImportGithub }: SkillsWorkspacePageProps) {
  const [query, setQuery] = useState('');
  const [categoryFilter, setCategoryFilter] = useState('all');
  const [marketplaceItems, setMarketplaceItems] = useState<OrganizationSkillCatalogItem[]>(marketplace);
  const [marketplaceTotal, setMarketplaceTotal] = useState(marketplace.length);
  const [marketplacePage, setMarketplacePage] = useState(1);
  const [marketplaceLoading, setMarketplaceLoading] = useState(false);
  const [view, setView] = useState<ViewKey>(() => {
    try {
      const stored = window.localStorage.getItem('himind-agent.skills-view');
      if (stored === 'marketplace' && marketEnabled) return 'marketplace';
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
  const items = status?.items || [];
	const installedById = useMemo(() => new Map(items.filter(item => ['installed', 'outdated', 'modified'].includes(item.client_state)).map(item => [item.record.manifest.id, item])), [items]);
	const localItems = useMemo(() => dashboardEnabled ? items.filter(item => item.client_state !== 'not_installed' && !isManagedSkill(item, desired, marketplace)) : items.filter(item => item.client_state !== 'not_installed'), [dashboardEnabled, desired, items, marketplace]);
	const systemSkillCount = useMemo(() => {
	  const ids = new Set((desired?.items || []).filter(item => item.asset_kind === 'skill' && isManagedPolicy(item)).map(item => item.asset_key));
	  items.filter(item => item.record.manifest.scope === 'builtin').forEach(item => ids.add(item.record.manifest.id));
	  marketplace.filter(item => isManagedCatalogSkill(item)).forEach(item => ids.add(item.skill_id));
	  return ids.size;
	}, [desired, items, marketplace]);
	const categoryCounts = useMemo(() => new Map(FUNCTIONAL_CATEGORIES.map(category => [category.id, marketplace.filter(item => functionalCategoryMatches(item.categories, category.id)).length])), [marketplace]);
	const visibleMarket = marketplaceItems;

  useEffect(() => {
    if (!marketEnabled || view !== 'marketplace') return;
    let active = true;
    const timer = window.setTimeout(async () => {
      setMarketplaceLoading(true);
      try {
        const result = await onQueryMarketplace(query.trim(), categoryFilter === 'all' ? '' : categoryFilter, 1, 50);
        if (!active) return;
        setMarketplaceItems(result.items || []);
        setMarketplaceTotal(result.total ?? result.items.length);
        setMarketplacePage(result.page || 1);
      } catch {
        if (!active) return;
        const normalized = query.trim().toLowerCase();
        const fallback = marketplace.filter(item => {
          if (normalized && ![item.skill_id, item.name, item.description, item.author_name, categorySearchText(item.categories), ...item.capability_ids].join(' ').toLowerCase().includes(normalized)) return false;
          if (categoryFilter !== 'all' && !functionalCategoryMatches(item.categories, categoryFilter)) return false;
          return true;
        });
        setMarketplaceItems(fallback);
        setMarketplaceTotal(fallback.length);
        setMarketplacePage(1);
      } finally {
        if (active) setMarketplaceLoading(false);
      }
    }, 180);
    return () => { active = false; window.clearTimeout(timer); };
  }, [categoryFilter, marketEnabled, marketplace, onQueryMarketplace, query, view]);

  async function loadMoreMarketplace() {
    if (marketplaceLoading || marketplaceItems.length >= marketplaceTotal) return;
    setMarketplaceLoading(true);
    try {
      const result = await onQueryMarketplace(query.trim(), categoryFilter === 'all' ? '' : categoryFilter, marketplacePage + 1, 50);
      setMarketplaceItems(current => [...current, ...(result.items || [])]);
      setMarketplaceTotal(result.total ?? marketplaceTotal);
      setMarketplacePage(result.page || marketplacePage + 1);
    } catch {
      // Keep the already loaded page visible when the next page is unavailable.
    } finally {
      setMarketplaceLoading(false);
    }
  }
  useEffect(() => {
	    const selectableIds = view === 'marketplace' ? marketplaceItems.map(item => item.skill_id) : view === 'installed' ? localItems.map(item => item.record.manifest.id) : [];
    if (!selectableIds.length) {
      setSelectedId('');
    } else if (!selectableIds.includes(selectedId)) {
      setSelectedId(selectableIds[0]);
    }
  }, [view, marketplaceItems, localItems, selectedId]);

  useEffect(() => { setDetailOpen(false); }, [view]);
  useEffect(() => { try { window.localStorage.setItem('himind-agent.skills-view', view); } catch { /* storage is optional */ } }, [view]);
  useEffect(() => {
    if ((!marketEnabled && view === 'marketplace') || (!dashboardEnabled && view === 'system')) setView('installed');
  }, [dashboardEnabled, marketEnabled, view]);

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
	  const installedCount = localItems.length;
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
        title="技能"
        description="安装和管理 AI 技能"
        actions={<>
          <button className="btn" title="从 GitHub 导入 Skill" onClick={() => { setGithubError(''); setGithubOpen(true); }}><Link2 size={15} />GitHub 导入</button>
          <button className="btn" title="导入本地 Skill" onClick={onImportLocal}><FolderOpen size={15} />导入本地 Skill</button>
          {view === 'installed' ? <button className="btn btn-primary" onClick={onSyncAll} disabled={isBusy || !items.length}><RefreshCw className={busyAction === 'sync-all' ? 'spin' : ''} size={16} />{busyAction === 'sync-all' ? '正在更新' : '更新全部'}</button> : null}
          {view === 'installed' ? <button className="btn btn-icon" title="打开技能目录" aria-label="打开技能目录" onClick={() => status?.target_root && onOpenDirectory(status.target_root)} disabled={!status?.target_root}><FolderOpen size={16} /></button> : null}
          <button className="btn btn-icon" title="刷新状态" aria-label="刷新状态" onClick={onRefresh} disabled={isBusy}><RefreshCw size={16} /></button>
        </>}
      />

      <SkillClientSummary status={status} />

      {error ? <div className="blocker"><CircleAlert size={18} /><div><strong>技能状态读取失败</strong><span>{error}</span></div></div> : null}

      {status?.target_mode === 'preview' && view !== 'marketplace' ? <div className="skill-inline-warning"><CircleAlert size={15} /><span>未找到 AI 工具的技能目录，当前安装仅在 HiMind Agent 中可用。</span></div> : null}
	  {marketEnabled && marketplaceError && view === 'marketplace' ? <div className="skill-inline-warning"><CircleAlert size={15} /><span>{marketplaceError}</span></div> : null}
	  <div className="plugin-toolbar skill-view-toolbar"><div className="plugin-tabs" role="tablist" aria-label="技能视图">
	    {marketEnabled ? <button role="tab" aria-selected={view === 'marketplace'} className={view === 'marketplace' ? 'active' : ''} onClick={() => setView('marketplace')}>市场 <span>{marketplace.length}</span></button> : null}
	    <button role="tab" aria-selected={view === 'installed'} className={view === 'installed' ? 'active' : ''} onClick={() => setView('installed')}>已安装 <span>{installedCount}</span></button>
	    {dashboardEnabled ? <button role="tab" aria-selected={view === 'system'} className={view === 'system' ? 'active' : ''} onClick={() => setView('system')}>受管理 <span>{systemSkillCount}</span></button> : null}
	  </div><div className="skill-sync-compact"><span>安装方式</span><div className="segmented-control" role="group" aria-label="技能安装方式"><button type="button" title="复制文件" aria-label="复制文件" aria-pressed={syncMode === 'copy'} className={syncMode === 'copy' ? 'active' : ''} disabled={isBusy} onClick={() => onSetSyncMode('copy')}><Files size={13} />复制</button><button type="button" title="软链接" aria-label="软链接" aria-pressed={syncMode === 'symlink'} className={syncMode === 'symlink' ? 'active' : ''} disabled={isBusy} onClick={() => onSetSyncMode('symlink')}><Link2 size={13} />链接</button></div></div></div>

      {dashboardEnabled && view === 'system' ? <ManagedCapabilitiesPanel assetKind="skill" desired={desired} loading={desiredLoading} error={desiredError} registry={pluginRegistry} skillStatus={status} /> : <section className={`skill-workspace ${view === 'marketplace' ? 'skill-marketplace-workspace' : ''} ${view === 'marketplace' && !visibleMarket.length ? 'catalog-empty-workspace' : ''} compact-master-detail ${detailOpen ? 'detail-open' : ''}`}>
        <aside className="skill-browser">
          <div className="skill-browser-tools">
            <label className="skill-search"><Search size={15} /><input value={query} onChange={event => setQuery(event.target.value)} placeholder="搜索技能" /></label>
          </div>
		  {marketEnabled && view === 'marketplace' ? <div className="market-category-block skill-marketplace-category"><div className="market-category-heading"><strong>功能分类</strong><span>按用途查找</span></div><label className="market-category-select"><span className="sr-only">技能功能分类</span><select value={categoryFilter} onChange={event => setCategoryFilter(event.target.value)}><option value="all">全部技能（{marketplace.length}）</option>{FUNCTIONAL_CATEGORIES.map(category => <option value={category.id} key={category.id}>{category.label}（{categoryCounts.get(category.id) || 0}）</option>)}</select></label><nav className="market-category-nav" aria-label="技能功能分类"><button type="button" className={categoryFilter === 'all' ? 'active' : ''} onClick={() => setCategoryFilter('all')}>全部技能<span>{marketplace.length}</span></button>{FUNCTIONAL_CATEGORIES.map(category => <button type="button" key={category.id} className={categoryFilter === category.id ? 'active' : ''} onClick={() => setCategoryFilter(category.id)}>{category.label}<span>{categoryCounts.get(category.id) || 0}</span></button>)}</nav></div> : null}
          {view === 'marketplace' ? <div className="plugin-catalog-result"><span>{marketplaceTotal} 个结果</span>{marketplaceLoading ? <span className="spinner" /> : null}</div> : null}
          <div className="skill-browser-list">
			{view === 'marketplace' ? visibleMarket.map(item => <MarketSkillListItem key={item.skill_id} item={item} installed={installedById.get(item.skill_id)} selected={item.skill_id === selectedMarket?.skill_id} onSelect={id => { setSelectedId(id); setDetailOpen(true); }} />) : filteredItems.map(item => <SkillListItem key={item.record.manifest.id} item={item} selected={item.record.manifest.id === selected?.record.manifest.id} onSelect={id => { setSelectedId(id); setDetailOpen(true); }} />)}
			{view === 'marketplace' && visibleMarket.length < marketplaceTotal ? <button className="plugin-load-more" disabled={marketplaceLoading} onClick={() => void loadMoreMarketplace()}>{marketplaceLoading ? '正在加载' : '加载更多'}</button> : null}
			{(view === 'marketplace' ? !visibleMarket.length && !marketplaceLoading : !filteredItems.length) ? <EmptyState icon={view === 'marketplace' ? Building2 : BookOpen} title={view === 'marketplace' ? (marketplace.length ? '没有匹配的技能' : '技能市场暂无内容') : '没有匹配的技能'} text={view === 'marketplace' ? (marketplace.length ? '调整搜索关键词或分类后重试。' : '审核通过并正式发布的 AI 技能会出现在这里。') : '调整搜索内容或筛选条件。'} /> : null}
          </div>
        </aside>

        <main className="skill-detail">
		  <button className="workspace-back" onClick={() => setDetailOpen(false)}><ArrowLeft size={15} />返回技能列表</button>
		  {view === 'marketplace' ? (selectedMarket ? <MarketSkillDetail item={selectedMarket} installed={installedById.get(selectedMarket.skill_id)} availablePlugins={availablePlugins} busyAction={busyAction} onLoadVersions={onLoadVersions} onPlan={(version) => void openInstallPlan(selectedMarket.skill_id, version)} planLoading={planLoading} /> : <EmptyState icon={Sparkles} title="选择一个技能" text="查看功能、依赖和版本。" />) : (selected ? <SkillDetail item={selected} availablePlugins={availablePlugins} catalogPolicy={marketplace.find(item => item.skill_id === selected.record.manifest.id)} busyAction={busyAction} onLoadVersions={onLoadVersions} onPlanVersion={(version) => void openInstallPlan(selected.record.manifest.id, version)} onSync={onSyncSkill} onRepair={onRepair} onUninstall={() => setPendingUninstall(selected)} onOpenDirectory={onOpenDirectory} /> : <EmptyState icon={Sparkles} title="选择一个技能" text="查看功能、依赖和版本。" />)}
        </main>
      </section>}

      {pendingUninstall ? <div className="skill-dialog-backdrop" role="presentation"><div className="skill-dialog" role="dialog" aria-modal="true" aria-labelledby="skill-uninstall-title">
        <div className="skill-dialog-head"><strong id="skill-uninstall-title">卸载技能</strong><button className="btn btn-icon" aria-label="关闭" onClick={() => setPendingUninstall(null)}><X size={16} /></button></div>
        <p>将从 Codex 中移除 <strong>{pendingUninstall.record.manifest.name}</strong>，本地版本会保留。</p>
        {pendingUninstall.modified_files.length ? <div className="skill-dialog-warning"><CircleAlert size={16} />检测到用户修改。请先使用“修复并备份”保留当前文件。</div> : null}
        <div className="skill-dialog-actions"><button className="btn" onClick={() => setPendingUninstall(null)}>取消</button><button className="btn btn-danger" disabled={isBusy || pendingUninstall.modified_files.length > 0} onClick={() => { onUninstall(pendingUninstall.record.manifest.id); setPendingUninstall(null); }}><Trash2 size={15} />确认卸载</button></div>
      </div></div> : null}
	  {installPlan || planError ? <InstallPlanDialog plan={installPlan} error={planError} currentVersion={installPlan ? installedById.get(installPlan.skill.skill_id)?.record.manifest.version : undefined} busy={isBusy} onClose={() => { setInstallPlan(null); setPlanError(''); }} onInstall={(optionalIds) => { if (installPlan) onInstallMarketplace(installPlan.skill.skill_id, installPlan.skill.version, optionalIds); setInstallPlan(null); }} /> : null}
	  {githubOpen ? <div className="modal-backdrop" role="presentation"><div className="modal" role="dialog" aria-modal="true" aria-labelledby="github-skill-title"><div className="modal-header"><div><h3 id="github-skill-title">从 GitHub 导入 Skill</h3><p>粘贴仓库链接即可；子目录和版本可按 UPM 方式写在链接中。</p></div><button className="btn btn-icon" aria-label="关闭" title="关闭" onClick={() => setGithubOpen(false)}><X size={16} /></button></div><div className="modal-body"><div className="field-group"><label className="field-label" htmlFor="github-skill-source-url">GitHub 链接</label><input id="github-skill-source-url" value={githubSourceUrl} onChange={event => setGithubSourceUrl(event.target.value)} placeholder="https://github.com/owner/repository.git?path=/skills/example#v1.0.0" /></div>{githubError ? <div className="inline-feedback visible" role="status">{githubError}</div> : null}<div className="modal-actions"><span /><div className="actions-row"><button className="btn" onClick={() => setGithubOpen(false)}>取消</button><button className="btn btn-primary" disabled={githubBusy || !githubSourceUrl.trim()} onClick={async () => { setGithubBusy(true); setGithubError(''); try { await onImportGithub(githubSourceUrl.trim()); setGithubOpen(false); } catch (error) { setGithubError(error instanceof Error ? error.message : 'GitHub Skill 导入失败'); } finally { setGithubBusy(false); } }}>{githubBusy ? '导入中...' : '导入 Skill'}</button></div></div></div></div></div> : null}
    </div>
  );
}

function MarketSkillListItem({ item, installed, selected, onSelect }: { item: OrganizationSkillCatalogItem; installed?: CodexSkillStatusItem; selected: boolean; onSelect: (id: string) => void }) {
  const update = installed ? compareSemanticVersions(item.version, installed.record.manifest.version) > 0 : false;
  const managed = item.assignment === 'required' && item.management !== 'user_managed';
  const label = item.assignment === 'blocked' ? '不可安装' : update ? '可更新' : installed ? '已安装' : managed ? '由组织管理' : item.assignment === 'recommended' ? '组织推荐' : '可安装';
  const tone = item.assignment === 'blocked' ? 'danger' : update ? 'warn' : installed ? 'success' : managed ? 'warn' : 'neutral';
  return <button className={`skill-browser-item skill-marketplace-item ${selected ? 'selected' : ''}`} onClick={() => onSelect(item.skill_id)}><span className={`skill-card-mark ${tone}`}>{item.name.slice(0, 1).toUpperCase()}</span><span className="skill-browser-item-copy"><strong>{item.name}</strong><small>{item.source === 'organization' ? '组织提供' : '公共技能库'} · {item.description || '暂无用途说明'}</small><small className="catalog-item-author">作者：{item.author_name || '马宝全'}</small></span><span className={`skill-state-label ${tone}`}>{label}</span></button>;
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
  const labels: Record<CodexSkillStatusItem['client_state'], string> = { not_installed: '未安装', installed: '已安装', outdated: '有更新', modified: '已修改', blocked: '不可用', unsupported: '不兼容', failed: '失败' };
  return labels[state] || state;
}

function stateTone(state: CodexSkillStatusItem['client_state']) { if (state === 'installed') return 'success'; if (state === 'outdated' || state === 'modified') return 'warn'; if (state === 'not_installed') return 'neutral'; return 'danger'; }
function statePill(state: CodexSkillStatusItem['client_state']): 'success' | 'warn' | 'danger' { if (state === 'installed') return 'success'; if (state === 'outdated' || state === 'modified' || state === 'not_installed') return 'warn'; return 'danger'; }
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
