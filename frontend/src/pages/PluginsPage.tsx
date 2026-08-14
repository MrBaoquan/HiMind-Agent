import { useEffect, useMemo, useState } from 'react';
import { AppWindow, ArrowLeft, Blocks, Bot, Download, ExternalLink, LockKeyhole, MonitorUp, MoreHorizontal, RefreshCw, Search, ShieldCheck, X } from 'lucide-react';
import type { CapabilityItem, CatalogPage, CodexSkillStatusResponse, ExtensionDesiredItem, ExtensionDesiredState, PluginCatalogItem, PluginInstallPlan, PluginItem, PluginRegistry } from '../services/agentApi';
import { EmptyState, PageHeader, Pill, Tags } from '../components/Common';
import { FUNCTIONAL_CATEGORIES, categorySearchText, functionalCategoryLabels, functionalCategoryMatches } from '../data/categoryCatalog';
import { ManagedCapabilitiesPanel } from './ManagedCapabilitiesPage';

const governanceLabels: Record<string, string> = {
  required: '系统内置',
  managed: '组织管理',
  optional: '可选插件',
  blocked: '不可安装',
};

export function PluginsPage({ loading, registry, catalog, desired, desiredLoading, desiredError, skillStatus, capabilities, onQueryCatalog, onRefresh, onLoadVersions, onPlanInstall, onInstall, onUninstall, onRollback, onSetEnabled, onOpenView, onCreateShortcut }: {
  loading: boolean;
  registry: PluginRegistry | null;
  catalog: PluginCatalogItem[];
  desired: ExtensionDesiredState | null;
  desiredLoading: boolean;
  desiredError: string | null;
  skillStatus: CodexSkillStatusResponse | null;
  capabilities: CapabilityItem[];
  onQueryCatalog: (q: string, category: string, page?: number, pageSize?: number) => Promise<CatalogPage<PluginCatalogItem>>;
  onLoadVersions: (pluginId: string) => Promise<PluginCatalogItem[]>;
  onPlanInstall: (pluginId: string, version?: string) => Promise<PluginInstallPlan>;
  onRefresh: () => void;
  onInstall: (pluginId: string, version?: string) => void;
  onUninstall: (pluginId: string) => void;
  onRollback: (pluginId: string) => void;
  onSetEnabled: (pluginId: string, enabled: boolean) => void;
  onOpenView: (pluginId: string, viewId: string) => void;
  onCreateShortcut: (pluginId: string, viewId: string, title: string) => void;
}) {
  const pluginItems = registry?.items || [];
  const userPluginItems = useMemo(() => pluginItems.filter(item => !isManagedPlugin(item, desired)), [desired, pluginItems]);
  const systemPluginCount = useMemo(() => {
    const ids = new Set((desired?.items || []).filter(item => item.asset_kind === 'plugin' && isManagedPolicy(item)).map(item => item.asset_key));
    pluginItems.filter(item => ['required', 'managed', 'blocked'].includes(item.governance || '')).forEach(item => ids.add(item.id));
    return ids.size;
  }, [desired, pluginItems]);
  const [selectedId, setSelectedId] = useState('');
  const [selectedMarketId, setSelectedMarketId] = useState('');
  const [detailOpen, setDetailOpen] = useState(false);
  const [view, setView] = useState<'market' | 'installed' | 'system'>(() => {
    try {
      const stored = window.localStorage.getItem('himind-agent.plugins-view');
      return stored === 'market' || stored === 'system' ? stored : 'installed';
    } catch { return 'installed'; }
  });
  const [query, setQuery] = useState('');
  const [categoryFilter, setCategoryFilter] = useState('all');
  const [marketCatalog, setMarketCatalog] = useState<PluginCatalogItem[]>(catalog);
  const [marketTotal, setMarketTotal] = useState(catalog.length);
  const [marketPage, setMarketPage] = useState(1);
  const [marketLoading, setMarketLoading] = useState(false);
  const [installPlan, setInstallPlan] = useState<PluginInstallPlan | null>(null);
  const [planError, setPlanError] = useState('');
  const [planningId, setPlanningId] = useState('');
  const selectedPlugin = useMemo(
    () => userPluginItems.find(item => item.id === selectedId) || userPluginItems[0] || null,
    [selectedId, userPluginItems],
  );
  const selectedCapabilities = useMemo(
    () => selectedPlugin ? capabilities.filter(item => item.source === `plugin:${selectedPlugin.id}` || selectedPlugin.capabilities?.some(capability => capability.id === item.id)) : [],
    [capabilities, selectedPlugin],
  );
  const installedById = new Map(pluginItems.map(item => [item.id, item]));
  const categoryCounts = useMemo(() => new Map(FUNCTIONAL_CATEGORIES.map(category => [category.id, catalog.filter(item => functionalCategoryMatches(item.categories, category.id)).length])), [catalog]);
  const filteredCatalog = marketCatalog;
  const visibleCatalog = filteredCatalog;
  const selectedMarket = filteredCatalog.find(item => item.plugin_id === selectedMarketId) || filteredCatalog[0] || null;

  useEffect(() => {
    if (view !== 'market') return;
    let active = true;
    const timer = window.setTimeout(async () => {
      setMarketLoading(true);
      try {
        const result = await onQueryCatalog(query.trim(), categoryFilter === 'all' ? '' : categoryFilter, 1, 50);
        if (!active) return;
        setMarketCatalog(result.items || []);
        setMarketTotal(result.total ?? result.items.length);
        setMarketPage(result.page || 1);
      } catch {
        if (!active) return;
        const normalized = query.trim().toLowerCase();
        const fallback = catalog.filter(item => {
          if (normalized && !`${item.name} ${item.plugin_id} ${item.description} ${item.author_name || ''} ${categorySearchText(item.categories)} ${(item.capability_ids || []).join(' ')}`.toLowerCase().includes(normalized)) return false;
          if (categoryFilter !== 'all' && !functionalCategoryMatches(item.categories, categoryFilter)) return false;
          return true;
        });
        setMarketCatalog(fallback);
        setMarketTotal(fallback.length);
        setMarketPage(1);
      } finally {
        if (active) setMarketLoading(false);
      }
    }, 180);
    return () => { active = false; window.clearTimeout(timer); };
  }, [catalog, categoryFilter, onQueryCatalog, query, view]);

  async function loadMoreMarket() {
    if (marketLoading || marketCatalog.length >= marketTotal) return;
    setMarketLoading(true);
    try {
      const result = await onQueryCatalog(query.trim(), categoryFilter === 'all' ? '' : categoryFilter, marketPage + 1, 50);
      setMarketCatalog(current => [...current, ...(result.items || [])]);
      setMarketTotal(result.total ?? marketTotal);
      setMarketPage(result.page || marketPage + 1);
    } catch {
      // Keep the already loaded page visible when the next page is unavailable.
    } finally {
      setMarketLoading(false);
    }
  }
  useEffect(() => { setDetailOpen(false); }, [view]);
  useEffect(() => { try { window.localStorage.setItem('himind-agent.plugins-view', view); } catch { /* storage is optional */ } }, [view]);

  async function openInstallPlan(pluginId: string, version?: string) {
    setPlanningId(pluginId);
    setPlanError('');
    try { setInstallPlan(await onPlanInstall(pluginId, version)); }
    catch { setPlanError('暂时无法检查插件，请稍后重试。'); }
    finally { setPlanningId(''); }
  }

  if (loading && !registry && catalog.length === 0) return <div className="page-loading"><span className="spinner" />正在加载插件</div>;

  return (
    <div className="plugin-page">
      <PageHeader title="插件" description="安装和管理本机功能" actions={<button className="btn btn-icon" title="刷新插件状态" aria-label="刷新插件状态" onClick={onRefresh}><RefreshCw size={16} /></button>} />
      <div className="plugin-toolbar"><div className="plugin-tabs" role="tablist" aria-label="插件视图"><button role="tab" aria-selected={view === 'market'} className={view === 'market' ? 'active' : ''} onClick={() => setView('market')}>市场 <span>{catalog.length}</span></button><button role="tab" aria-selected={view === 'installed'} className={view === 'installed' ? 'active' : ''} onClick={() => setView('installed')}>已安装 <span>{userPluginItems.length}</span></button><button role="tab" aria-selected={view === 'system'} className={view === 'system' ? 'active' : ''} onClick={() => setView('system')}>受管理 <span>{systemPluginCount}</span></button></div><div className="plugin-toolbar-meta"><span className={`status-dot ${registry?.registry_ready ? 'success' : 'danger'}`} /><span>{registry?.registry_ready ? '插件可用' : '插件需要处理'}</span></div></div>
      {view === 'market' ? <section className={`plugin-catalog-workspace ${!visibleCatalog.length ? 'catalog-empty-workspace' : ''} compact-master-detail ${detailOpen ? 'detail-open' : ''}`}>
        <aside className="plugin-catalog-browser">
          <div className="plugin-catalog-tools"><label className="plugin-search"><Search size={15} /><span className="sr-only">搜索插件</span><input value={query} onChange={event => setQuery(event.target.value)} placeholder="搜索名称或用途" /></label></div>
          <div className="market-category-block"><div className="market-category-heading"><strong>功能分类</strong><span>按用途查找</span></div><label className="market-category-select"><span className="sr-only">插件功能分类</span><select value={categoryFilter} onChange={event => setCategoryFilter(event.target.value)}><option value="all">全部插件（{catalog.length}）</option>{FUNCTIONAL_CATEGORIES.map(category => <option value={category.id} key={category.id}>{category.label}（{categoryCounts.get(category.id) || 0}）</option>)}</select></label><nav className="market-category-nav" aria-label="插件功能分类"><button type="button" className={categoryFilter === 'all' ? 'active' : ''} onClick={() => setCategoryFilter('all')}>全部插件<span>{catalog.length}</span></button>{FUNCTIONAL_CATEGORIES.map(category => <button type="button" key={category.id} className={categoryFilter === category.id ? 'active' : ''} onClick={() => setCategoryFilter(category.id)}>{category.label}<span>{categoryCounts.get(category.id) || 0}</span></button>)}</nav></div>
          <div className="plugin-catalog-result"><span>{marketTotal} 个结果</span>{marketLoading ? <span className="spinner" /> : null}</div>
          <div className="plugin-catalog-list">{visibleCatalog.map(item => { const installed = installedById.get(item.plugin_id); const upgrade = installed?.version ? compareSemanticVersions(item.version, installed.version) > 0 : false; return <button key={item.plugin_id} className={`plugin-catalog-item ${selectedMarket?.plugin_id === item.plugin_id ? 'selected' : ''}`} onClick={() => { setSelectedMarketId(item.plugin_id); setDetailOpen(true); }}><span className="plugin-card-mark">{item.name.slice(0, 1)}</span><span><strong>{item.name}</strong><small>{item.description || catalogSourceLabel(item.source)}</small><small className="catalog-item-author">作者：{item.author_name || '未知作者'}</small></span><span className={`skill-state-label ${upgrade ? 'warn' : installed ? 'success' : 'neutral'}`}>{upgrade ? '可更新' : installed ? '已安装' : catalogAssignmentLabel(item)}</span></button>; })}{visibleCatalog.length < marketTotal ? <button className="plugin-load-more" disabled={marketLoading} onClick={() => void loadMoreMarket()}>{marketLoading ? '正在加载' : '加载更多'}</button> : null}{!visibleCatalog.length && !marketLoading ? <EmptyState icon={Search} title="没有匹配的插件" text={catalog.length ? '调整关键词或筛选条件。' : '插件库暂无内容。'} /> : null}</div>
        </aside>
        <main className="plugin-catalog-detail"><button className="workspace-back" onClick={() => setDetailOpen(false)}><ArrowLeft size={15} />返回插件列表</button>{selectedMarket ? <MarketPluginDetail item={selectedMarket} installed={installedById.get(selectedMarket.plugin_id)} catalog={catalog} planning={planningId === selectedMarket.plugin_id} onLoadVersions={onLoadVersions} onPlan={(version) => void openInstallPlan(selectedMarket.plugin_id, version)} /> : <EmptyState icon={Blocks} title="选择一个插件" text="查看功能、依赖和版本。" />}</main>
      </section> : view === 'system' ? <ManagedCapabilitiesPanel assetKind="plugin" desired={desired} loading={desiredLoading} error={desiredError} registry={registry} skillStatus={skillStatus} /> : <>
      <div className={`plugin-workspace compact-master-detail ${detailOpen ? 'detail-open' : ''}`}>
        <aside className="plugin-list" aria-label="本机插件列表">
          <div className="plugin-list-header"><strong>已安装</strong><span className="section-count">{userPluginItems.length}</span></div>
          <div className="plugin-list-body">
            {userPluginItems.map(item => (
              <button key={item.id} type="button" className={`plugin-list-item ${selectedPlugin?.id === item.id ? 'selected' : ''}`} onClick={() => { setSelectedId(item.id); setDetailOpen(true); }}>
                <span className={`status-dot ${item.status === 'failed' ? 'danger' : item.enabled ? 'success' : ''}`} />
                <span><strong>{item.name || item.id}</strong><small>作者：{item.author_name || catalog.find(candidate => candidate.plugin_id === item.id)?.author_name || '未知作者'}</small><small>{item.circuit_open || item.status === 'failed' ? '需要处理' : item.enabled ? '已启用' : '已停用'}</small></span>
                <small>v{item.version || '--'}</small>
              </button>
            ))}
            {userPluginItems.length === 0 ? <EmptyState icon={Blocks} title="暂无本机插件" text="系统内置和组织管理插件已移到“受管理”分类。" /> : null}
          </div>
        </aside>
      <main className="plugin-detail">
          <button className="workspace-back" onClick={() => setDetailOpen(false)}><ArrowLeft size={15} />返回插件列表</button>
          {selectedPlugin ? <PluginDetail key={selectedPlugin.id} item={selectedPlugin} capabilities={selectedCapabilities} catalog={catalog} onLoadVersions={onLoadVersions} onPlanVersion={(version) => void openInstallPlan(selectedPlugin.id, version)} onUninstall={onUninstall} onRollback={onRollback} onSetEnabled={onSetEnabled} onOpenView={onOpenView} onCreateShortcut={onCreateShortcut} /> : <EmptyState icon={Blocks} title="选择插件" text="查看功能、依赖和版本。" />}
        </main>
      </div></>}
      {installPlan || planError ? <PluginInstallPlanDialog plan={installPlan} error={planError} currentVersion={installPlan ? installedById.get(installPlan.plugin.plugin_id)?.version : undefined} onClose={() => { setInstallPlan(null); setPlanError(''); }} onInstall={() => { if (installPlan) onInstall(installPlan.plugin.plugin_id, installPlan.plugin.version); setInstallPlan(null); }} /> : null}
    </div>
  );
}

function MarketPluginDetail({ item, installed, catalog, planning, onLoadVersions, onPlan }: { item: PluginCatalogItem; installed?: PluginItem; catalog: PluginCatalogItem[]; planning: boolean; onLoadVersions: (pluginId: string) => Promise<PluginCatalogItem[]>; onPlan: (version?: string) => void }) {
  const upgrade = installed?.version ? compareSemanticVersions(item.version, installed.version) > 0 : false;
  const current = Boolean(installed) && !upgrade;
  const managed = item.assignment === 'required' && item.management !== 'user_managed';
  const lockedLabel = item.governance === 'blocked' ? '不可安装' : managed ? '组织管理' : undefined;
  const permissions = friendlyPermissions(item.permissions || []);
  const [tab, setTab] = useState<'details' | 'versions'>('details');
  const [versions, setVersions] = useState<PluginCatalogItem[]>([item]);
  const [versionsLoading, setVersionsLoading] = useState(false);
  const [versionsError, setVersionsError] = useState('');

  useEffect(() => {
    if (tab !== 'versions') return;
    let active = true;
    setVersionsLoading(true);
    setVersionsError('');
    onLoadVersions(item.plugin_id).then(result => { if (active) setVersions(result.length ? result : [item]); }).catch(() => { if (active) setVersionsError('暂时无法读取版本，请稍后重试。'); }).finally(() => { if (active) setVersionsLoading(false); });
    return () => { active = false; };
  }, [item, onLoadVersions, tab]);

  return <>
      <header className="plugin-product-header"><div className="plugin-product-title"><span className="plugin-product-mark">{item.name.slice(0, 1)}</span><div><div><h3>{item.name}</h3><Pill kind={item.governance === 'blocked' ? 'danger' : managed ? 'warn' : 'success'}>{catalogAssignmentLabel(item)}</Pill></div><span>{catalogSourceLabel(item.source)} · {item.author_name || '未知作者'}</span></div></div><button className="btn btn-primary" disabled={current || managed || item.governance === 'blocked' || planning} onClick={() => onPlan(item.version)}><Download size={15} />{planning ? '正在检查' : item.governance === 'blocked' ? '不可安装' : managed ? '组织管理' : upgrade ? '更新' : current ? '已安装' : '安装'}</button></header>
    <p className="plugin-product-description">{item.description || '暂无用途说明。'}</p>
    {item.organization_reason ? <div className="plugin-product-notice"><ShieldCheck size={16} /><div><strong>组织说明</strong><span>{item.organization_reason}</span></div></div> : null}
    <DetailTabs value={tab} onChange={setTab} />
    {tab === 'versions' ? <PluginVersionList versions={versions} currentVersion={installed?.version} lockedLabel={lockedLabel} loading={versionsLoading} error={versionsError} onSelect={onPlan} /> : <>
      <section className="plugin-product-section"><div className="plugin-section-heading"><div><h4>功能</h4></div></div><div className="plugin-feature-list"><div><AppWindow size={17} /><span><strong>桌面工具</strong><small>{item.view_count ? '可在独立窗口中使用' : '无独立窗口'}</small></span></div><div><Bot size={17} /><span><strong>AI 工具</strong><small>{item.capability_ids?.length ? '可供 AI 调用' : '不提供 AI 工具'}</small></span></div></div><div className="market-taxonomy"><Tags items={functionalCategoryLabels(item.categories)} /></div></section>
      <section className="plugin-product-section"><div className="plugin-section-heading"><div><h4>依赖</h4></div></div><div className="plugin-product-dependencies">{(item.plugin_dependencies || []).map(dependency => <div key={dependency.plugin_id}><span className="status-dot success" /><PluginDependencyName pluginId={dependency.plugin_id} catalog={catalog} /><span>{dependency.required ? '必需' : '可选'}</span><strong>{dependency.min_version ? `v${dependency.min_version} 及以上` : '不限版本'}</strong></div>)}{!item.plugin_dependencies?.length ? <div className="plugin-section-empty">无依赖</div> : null}</div></section>
      {permissions.length ? <section className="plugin-product-section"><div className="plugin-section-heading"><div><h4>权限</h4></div></div><div className="plugin-permission-friendly"><Tags items={permissions} /></div></section> : null}
      <details className="plugin-technical-panel"><summary>开发者信息</summary><div className="plugin-technical-grid"><div><span>插件 ID</span><code>{item.plugin_id}</code></div><div><span>最低 Agent 版本</span><strong>{item.min_agent_version ? `v${item.min_agent_version}` : '--'}</strong></div><div><span>安装包大小</span><strong>{formatFileSize(item.file_size)}</strong></div><div><span>当前版本</span><strong>v{item.version}</strong></div></div></details>
    </>}
  </>;
}

function DetailTabs({ value, onChange }: { value: 'details' | 'versions'; onChange: (value: 'details' | 'versions') => void }) {
  return <div className="extension-detail-tabs" role="tablist"><button role="tab" aria-selected={value === 'details'} className={value === 'details' ? 'active' : ''} onClick={() => onChange('details')}>详情</button><button role="tab" aria-selected={value === 'versions'} className={value === 'versions' ? 'active' : ''} onClick={() => onChange('versions')}>版本</button></div>;
}

function PluginVersionList({ versions, currentVersion, lockedLabel, loading, error, onSelect }: { versions: PluginCatalogItem[]; currentVersion?: string; lockedLabel?: string; loading: boolean; error: string; onSelect: (version: string) => void }) {
  const sorted = [...versions].sort((left, right) => compareSemanticVersions(right.version, left.version));
  return <section className="extension-version-list">{loading ? <div className="extension-version-empty"><span className="spinner" />正在读取版本</div> : null}{error ? <div className="plugin-local-error">{error}</div> : null}{!loading && sorted.map(version => { const installed = currentVersion === version.version; const newer = currentVersion ? compareSemanticVersions(version.version, currentVersion) > 0 : false; const action = installed ? '已安装' : lockedLabel || (currentVersion ? newer ? '更新' : '切换' : '安装'); return <article className="extension-version-row" key={version.version}><div className="extension-version-main"><div><strong>v{version.version}</strong>{installed ? <Pill kind="success">已安装</Pill> : null}</div><time>{formatPublishedAt(version.published_at)}</time><p>{version.release_notes || '未提供更新说明。'}</p></div><button className={installed || lockedLabel ? 'btn' : 'btn btn-primary'} disabled={installed || Boolean(lockedLabel)} onClick={() => onSelect(version.version)}>{action}</button></article>; })}</section>;
}

function PluginDependencyName({ pluginId, catalog }: { pluginId: string; catalog: PluginCatalogItem[] }) {
  const plugin = catalog.find(item => item.plugin_id === pluginId);
  return <span className="plugin-dependency-name"><strong>{plugin?.name || readablePluginID(pluginId)}</strong></span>;
}

function PluginInstallPlanDialog({ plan, error, currentVersion, onClose, onInstall }: { plan: PluginInstallPlan | null; error: string; currentVersion?: string; onClose: () => void; onInstall: () => void }) {
  const action = !plan || !currentVersion ? '安装' : compareSemanticVersions(plan.plugin.version, currentVersion) > 0 ? '更新' : '切换版本';
  const title = action === '切换版本' ? '切换插件版本' : `${action}插件`;
  return <div className="skill-dialog-backdrop"><div className="skill-dialog skill-plan-dialog" role="dialog" aria-modal="true"><div className="skill-dialog-head"><strong>{title}</strong><button className="btn btn-icon" onClick={onClose} aria-label="关闭"><X size={16} /></button></div>{error ? <div className="skill-dialog-warning">{error}</div> : null}{plan ? <><div className="skill-plan-summary"><strong>{plan.plugin.name} v{plan.plugin.version}</strong><span>{plan.ready ? '可以安装' : '当前无法安装'}</span></div><div className="skill-plan-actions">{plan.dependency_actions.map(action => <div className={`plugin-plan-row ${['blocked', 'unavailable'].includes(action.action) ? 'blocked' : ''}`} key={action.plugin_id}><span className={`status-dot ${action.action === 'satisfied' ? 'success' : ['blocked', 'unavailable'].includes(action.action) ? 'danger' : ''}`} /><span><strong>{action.plugin_name || readablePluginID(action.plugin_id)}</strong><small>{pluginInstallActionDescription(action.action)}</small></span><strong>{action.target_version ? `v${action.target_version}` : '—'}</strong></div>)}{!plan.dependency_actions.length ? <span className="skill-section-empty">无依赖</span> : null}</div></> : null}<div className="skill-dialog-actions"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-primary" disabled={!plan?.ready} onClick={onInstall}><Download size={15} />确认{action}</button></div></div></div>;
}

function PluginDetail({ item, capabilities, catalog, onLoadVersions, onPlanVersion, onUninstall, onRollback, onSetEnabled, onOpenView, onCreateShortcut }: {
  item: PluginItem;
  capabilities: CapabilityItem[];
  catalog: PluginCatalogItem[];
  onLoadVersions: (pluginId: string) => Promise<PluginCatalogItem[]>;
  onPlanVersion: (version: string) => void;
  onUninstall: (pluginId: string) => void;
  onRollback: (pluginId: string) => void;
  onSetEnabled: (pluginId: string, enabled: boolean) => void;
  onOpenView: (pluginId: string, viewId: string) => void;
  onCreateShortcut: (pluginId: string, viewId: string, title: string) => void;
}) {
  const [pendingAction, setPendingAction] = useState<{ title: string; description: string; confirmText: string; run: () => void } | null>(null);
  const [tab, setTab] = useState<'details' | 'versions'>('details');
  const [versions, setVersions] = useState<PluginCatalogItem[]>(() => catalog.filter(candidate => candidate.plugin_id === item.id));
  const [versionsLoading, setVersionsLoading] = useState(false);
  const [versionsError, setVersionsError] = useState('');

  const friendlyPermissionItems = friendlyPermissions(item.permissions || []);
  const catalogItem = catalog.find(candidate => candidate.plugin_id === item.id);
  const managed = item.governance === 'required' || item.governance === 'managed' || catalogItem?.management !== 'user_managed' && Boolean(catalogItem?.managed);

  useEffect(() => {
    if (item.development || tab !== 'versions') return;
    let active = true;
    setVersionsLoading(true);
    setVersionsError('');
    onLoadVersions(item.id).then(result => { if (active) setVersions(result); }).catch(() => { if (active) setVersionsError('暂时无法读取版本，请稍后重试。'); }).finally(() => { if (active) setVersionsLoading(false); });
    return () => { active = false; };
  }, [item.development, item.id, onLoadVersions, tab]);

  return (
    <>
      <div className="plugin-detail-header">
        <div><div className="plugin-title-line"><h3>{item.name || item.id}</h3><Pill kind={item.circuit_open ? 'danger' : item.status === 'installed' && item.enabled ? 'success' : item.status === 'failed' ? 'danger' : 'warn'}>{item.circuit_open || item.status === 'failed' ? '运行异常' : item.enabled ? '已安装' : '已停用'}</Pill>{item.development ? <Pill kind="warn">开发中</Pill> : null}{item.governance ? <Pill kind={item.governance === 'required' || item.governance === 'managed' ? 'warn' : 'success'}>{governanceLabels[item.governance] || item.governance}</Pill> : null}</div></div>
        <div className="plugin-installed-actions">{!item.development ? <><label className="plugin-enable-control"><span>启用</span><span className="toggle"><input type="checkbox" checked={Boolean(item.enabled)} disabled={item.governance === 'required' || item.governance === 'managed'} onChange={event => event.target.checked ? onSetEnabled(item.id, true) : setPendingAction({ title: '确认停用插件？', description: `停用后，Agent 将不再调用“${item.name || item.id}”提供的能力。`, confirmText: '确认停用', run: () => onSetEnabled(item.id, false) })} /><span className="slider" /></span></label><details className="plugin-more-actions"><summary title="更多操作" aria-label="更多操作"><MoreHorizontal size={17} /></summary><div>{item.governance !== 'required' && item.governance !== 'managed' && item.governance !== 'blocked' ? <button disabled={!item.rollback_available} onClick={() => setPendingAction({ title: '确认回滚插件？', description: `将“${item.name || item.id}”切换到 v${item.previous_version || '--'}，当前版本会停止运行。`, confirmText: '确认回滚', run: () => onRollback(item.id) })}>回滚{item.previous_version ? `到 v${item.previous_version}` : ''}</button> : null}<button className="danger-text" disabled={item.governance === 'required' || item.governance === 'managed'} onClick={() => setPendingAction({ title: '确认卸载插件？', description: `卸载后将移除“${item.name || item.id}”，其能力和功能页面会立即不可用。`, confirmText: '确认卸载', run: () => onUninstall(item.id) })}>卸载</button></div></details></> : null}</div>
      </div>
      {item.error ? <div className="plugin-local-error">插件暂时无法运行，请刷新状态或重新安装。</div> : null}
      {item.development ? <div className="plugin-product-notice"><ShieldCheck size={16} /><div><strong>本机开发版本</strong><span>项目构建、测试和移除统一在“扩展开发”中管理。</span></div></div> : null}
      <p className="plugin-product-description">{item.description || '暂无用途说明。'}</p>
      {!item.development ? <DetailTabs value={tab} onChange={setTab} /> : null}
      {!item.development && tab === 'versions' ? <PluginVersionList versions={versions.length ? versions : catalogItem ? [catalogItem] : []} currentVersion={item.version} lockedLabel={item.governance === 'blocked' ? '不可安装' : managed ? '组织管理' : undefined} loading={versionsLoading} error={versionsError} onSelect={onPlanVersion} /> : <>
        {item.development ? <div className="plugin-meta-grid"><div><span>版本</span><strong>v{item.version || '--'}</strong></div><div><span>构建时间</span><strong>{formatBuildTime(item.entry_modified_at)}</strong></div><div><span>入口大小</span><strong>{formatFileSize(item.entry_size)}</strong></div></div> : null}
        <section className="plugin-detail-section">
          <div className="plugin-section-heading"><div><h4>功能</h4></div></div>
          <div className="plugin-view-list">
            {item.views?.map(view => <div className="plugin-view-row" key={view.id}><MonitorUp size={17} /><div><strong>{view.title}</strong></div><div className="actions-row"><button className="btn btn-primary" onClick={() => onOpenView(item.id, view.id)}><ExternalLink size={15} />打开</button><button className="btn" onClick={() => onCreateShortcut(item.id, view.id, view.title)}>创建快捷方式</button></div></div>)}
            {capabilities.length ? <div className="plugin-view-row"><Bot size={17} /><div><strong>AI 工具</strong><small>可供 AI 使用</small></div></div> : null}
            {!item.views?.length && !capabilities.length ? <div className="plugin-section-empty">未声明可用功能</div> : null}
          </div>
        </section>
        <section className="plugin-detail-section"><div className="plugin-section-heading"><div><h4>依赖</h4></div></div><div className="plugin-product-dependencies">{(item.plugin_dependencies || []).map(dependency => <div key={dependency.plugin_id}><span className="status-dot success" /><PluginDependencyName pluginId={dependency.plugin_id} catalog={catalog} /><span>{dependency.required ? '必需' : '可选'}</span><strong>{dependency.min_version ? `v${dependency.min_version} 及以上` : '不限版本'}</strong></div>)}{!item.plugin_dependencies?.length ? <div className="plugin-section-empty">无依赖</div> : null}</div></section>
        {friendlyPermissionItems.length ? <section className="plugin-detail-section"><div className="plugin-section-heading"><div><h4>权限</h4></div></div><div className="plugin-permission-list"><Tags items={friendlyPermissionItems} /></div></section> : null}
        {!item.development ? <details className="plugin-technical-panel"><summary>开发者信息</summary><div className="plugin-technical-grid"><div><span>插件 ID</span><code>{item.id}</code></div><div><span>作者</span><strong>{item.author_name || catalogItem?.author_name || '未知作者'}</strong></div><div><span>运行时</span><strong>{item.runtime || '--'}</strong></div><div><span>最低 Agent 版本</span><strong>{item.min_agent_version ? `v${item.min_agent_version}` : '--'}</strong></div></div></details> : null}
      </>}
      {pendingAction ? <ConfirmPluginAction action={pendingAction} onClose={() => setPendingAction(null)} /> : null}
    </>
  );
}

function ConfirmPluginAction({ action, onClose }: { action: { title: string; description: string; confirmText: string; run: () => void }; onClose: () => void }) {
  return <div className="modal-backdrop" role="presentation"><div className="modal" role="dialog" aria-modal="true" aria-labelledby="plugin-action-title"><div className="modal-header"><div><h3 id="plugin-action-title">{action.title}</h3><p>{action.description}</p></div><button className="btn btn-icon" aria-label="关闭" onClick={onClose}><X size={16} /></button></div><div className="modal-body"><div className="modal-actions"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-danger" onClick={() => { action.run(); onClose(); }}>{action.confirmText}</button></div></div></div></div>;
}

function formatBuildTime(value?: number) {
  return value ? new Date(value).toLocaleString('zh-CN', { hour12: false }) : '--';
}

function formatFileSize(value?: number) {
  if (value === undefined) return '--';
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / 1024 / 1024).toFixed(1)} MB`;
}

function formatPublishedAt(value?: string) {
  if (!value) return '发布时间未知';
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleDateString('zh-CN');
}

function catalogSourceLabel(source?: string) {
  if (source === 'system') return '系统内置';
  if (source === 'organization') return '组织';
  return '公共插件库';
}

function catalogAssignmentLabel(item: PluginCatalogItem) {
  if (item.assignment === 'blocked' || item.governance === 'blocked') return '不可安装';
  if (item.source === 'system') return '系统内置';
  if (item.assignment === 'required') return item.management === 'builtin' ? '系统内置' : '组织必装';
  if (item.assignment === 'recommended') return '组织推荐';
  return '可选插件';
}

function isManagedPolicy(item: ExtensionDesiredItem) {
  return item.management !== 'user_managed' || item.intent === 'required' || item.desired_state === 'absent';
}

function isManagedPlugin(item: PluginItem, desired: ExtensionDesiredState | null) {
  if (['required', 'managed', 'blocked'].includes(item.governance || '')) return true;
  return Boolean(desired?.items.some(policy => policy.asset_kind === 'plugin' && policy.asset_key === item.id && isManagedPolicy(policy)));
}

function readablePluginID(value: string) { const tail = value.split('.').filter(Boolean).pop() || value; return tail.split(/[-_]/).filter(Boolean).map(part => part.charAt(0).toUpperCase() + part.slice(1)).join(' '); }
function pluginInstallActionDescription(action: string) { return ({ satisfied: '已安装', install: '将一并安装', update: '将一并更新', blocked: '被组织策略阻止', unavailable: '当前不可用' } as Record<string, string>)[action] || '需要处理'; }

function friendlyPermissions(values: string[]) {
  const labels = values.map(value => {
    const normalized = value.toLowerCase();
    let scope = '其他设备能力';
    if (normalized.startsWith('secret.')) scope = '受保护凭据';
    else if (normalized.startsWith('network.')) scope = '网络访问';
    else if (normalized.startsWith('filesystem.') || normalized.startsWith('fs.') || normalized.startsWith('artifact.')) scope = '文件访问';
    else if (normalized.startsWith('process.')) scope = '本机程序';
    else if (normalized.startsWith('shell.')) scope = '命令执行';
    else if (normalized.startsWith('clipboard.')) scope = '剪贴板';
    if (normalized.endsWith('.read')) return `${scope}（读取）`;
    if (normalized.endsWith('.write')) return `${scope}（写入）`;
    if (normalized.endsWith('.broker')) return `${scope}（受控）`;
    return scope;
  });
  return [...new Set(labels)];
}

function compareSemanticVersions(left: string, right: string) {
  const parse = (value: string) => value.split(/[.+-]/).slice(0, 3).map(part => Number.parseInt(part, 10) || 0);
  const leftParts = parse(left);
  const rightParts = parse(right);
  for (let index = 0; index < 3; index += 1) {
    if ((leftParts[index] || 0) !== (rightParts[index] || 0)) return (leftParts[index] || 0) - (rightParts[index] || 0);
  }
  return 0;
}

function riskLevelLabel(value?: string) { return ({ low: '低风险', medium: '中风险', high: '高风险' } as Record<string, string>)[value || ''] || (value || '未声明'); }
