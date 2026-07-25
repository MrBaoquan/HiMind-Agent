import { useEffect, useMemo, useState } from 'react';
import { AppWindow, Blocks, Bot, CheckCircle2, Download, ExternalLink, FolderOpen, FlaskConical, LockKeyhole, MonitorUp, MoreHorizontal, PackagePlus, Play, RefreshCw, Search, Send, ShieldCheck, Unplug, X } from 'lucide-react';
import type { AuthoringPluginDraft, CapabilityItem, DevelopmentInvocationResult, PluginCapability, PluginCatalogItem, PluginInstallPlan, PluginItem, PluginJsonSchema, PluginRegistry, PluginSubmissionStatus } from '../services/agentApi';
import { EmptyState, PageHeader, Pill, Tags } from '../components/Common';
import { FUNCTIONAL_CATEGORIES, categorySearchText, functionalCategoryLabels, functionalCategoryMatches } from '../data/categoryCatalog';

const governanceLabels: Record<string, string> = {
  required: '系统内置',
  managed: '组织提供',
  optional: '自行安装',
  blocked: '组织已禁止',
};

export function PluginsPage({ loading, registry, catalog, capabilities, drafts, submissions, busyAction, onRefresh, onPlanInstall, onInstall, onUninstall, onRollback, onSetEnabled, onOpenDirectory, onRegisterDevelopment, onImportCandidate, onCreateRevision, onTestDraft, onConfirmDraft, onSubmitDraft, onUnregisterDevelopment, onInvokeDevelopment, onOpenView, onCreateShortcut }: {
  loading: boolean;
  registry: PluginRegistry | null;
  catalog: PluginCatalogItem[];
  capabilities: CapabilityItem[];
  drafts: AuthoringPluginDraft[];
  submissions: PluginSubmissionStatus[];
  busyAction: string | null;
  onPlanInstall: (pluginId: string) => Promise<PluginInstallPlan>;
  onRefresh: () => void;
  onInstall: (pluginId: string) => void;
  onUninstall: (pluginId: string) => void;
  onRollback: (pluginId: string) => void;
  onSetEnabled: (pluginId: string, enabled: boolean) => void;
  onOpenDirectory: () => void;
  onRegisterDevelopment: () => void;
  onImportCandidate: () => void;
  onCreateRevision: (pluginId: string, version: string) => void;
  onTestDraft: (pluginId: string, version: string) => void;
  onConfirmDraft: (pluginId: string, version: string) => void;
  onSubmitDraft: (pluginId: string, version: string) => void;
  onUnregisterDevelopment: (pluginId: string) => void;
  onInvokeDevelopment: (pluginId: string, capabilityId: string, input: unknown) => Promise<DevelopmentInvocationResult>;
  onOpenView: (pluginId: string, viewId: string) => void;
  onCreateShortcut: (pluginId: string, viewId: string, title: string) => void;
}) {
  const pluginItems = registry?.items || [];
  const [selectedId, setSelectedId] = useState('');
  const [selectedMarketId, setSelectedMarketId] = useState('');
  const [view, setView] = useState<'market' | 'installed' | 'mine'>('market');
  const [selectedDraftId, setSelectedDraftId] = useState('');
  const [query, setQuery] = useState('');
  const [categoryFilter, setCategoryFilter] = useState('all');
  const [visibleLimit, setVisibleLimit] = useState(50);
  const [installPlan, setInstallPlan] = useState<PluginInstallPlan | null>(null);
  const [planError, setPlanError] = useState('');
  const [planningId, setPlanningId] = useState('');
  const selectedPlugin = useMemo(
    () => pluginItems.find(item => item.id === selectedId) || pluginItems[0] || null,
    [pluginItems, selectedId],
  );
  const selectedCapabilities = useMemo(
    () => selectedPlugin ? capabilities.filter(item => item.source === `plugin:${selectedPlugin.id}` || selectedPlugin.capabilities?.some(capability => capability.id === item.id)) : [],
    [capabilities, selectedPlugin],
  );
  const installedById = new Map(pluginItems.map(item => [item.id, item]));
  const categoryCounts = useMemo(() => new Map(FUNCTIONAL_CATEGORIES.map(category => [category.id, catalog.filter(item => functionalCategoryMatches(item.categories, category.id)).length])), [catalog]);
  const filteredCatalog = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    return catalog.filter(item => {
      if (normalized && !`${item.name} ${item.plugin_id} ${item.description} ${item.author_name || ''} ${categorySearchText(item.categories)} ${(item.capability_ids || []).join(' ')}`.toLowerCase().includes(normalized)) return false;
      if (categoryFilter !== 'all' && !functionalCategoryMatches(item.categories, categoryFilter)) return false;
      return true;
    });
  }, [catalog, categoryFilter, query]);
  const visibleCatalog = filteredCatalog.slice(0, visibleLimit);
  const selectedMarket = filteredCatalog.find(item => item.plugin_id === selectedMarketId) || filteredCatalog[0] || null;
  const selectedDraft = drafts.find(item => `${item.manifest.id}@${item.manifest.version}` === selectedDraftId) || drafts[0] || null;

  useEffect(() => { setVisibleLimit(50); }, [query, categoryFilter]);

  async function openInstallPlan(pluginId: string) {
    setPlanningId(pluginId);
    setPlanError('');
    try { setInstallPlan(await onPlanInstall(pluginId)); }
    catch (reason) { setPlanError(reason instanceof Error ? reason.message : String(reason)); }
    finally { setPlanningId(''); }
  }

  if (loading && !registry && catalog.length === 0) return <div className="page-loading"><span className="spinner" />正在读取插件市场与本机注册表</div>;

  return (
    <div className="plugin-page">
      <PageHeader title="插件" description="工具与本机能力" actions={<>{view === 'mine' ? <button className="btn btn-primary" disabled={Boolean(busyAction)} onClick={onImportCandidate}><PackagePlus size={16} />导入候选包</button> : null}<button className="btn btn-icon" title="刷新插件状态" aria-label="刷新插件状态" onClick={onRefresh}><RefreshCw size={16} /></button><details className="plugin-developer-tools"><summary><FlaskConical size={15} />开发者工具</summary><div><button className="btn" onClick={onRegisterDevelopment}>加载未打包工程</button><button className="btn" onClick={onOpenDirectory} disabled={!registry?.registry_dir}><FolderOpen size={15} />打开插件目录</button></div></details></>} />
      <div className="plugin-toolbar"><div className="plugin-tabs" role="tablist" aria-label="插件视图"><button role="tab" aria-selected={view === 'market'} className={view === 'market' ? 'active' : ''} onClick={() => setView('market')}>插件市场 <span>{catalog.length}</span></button><button role="tab" aria-selected={view === 'installed'} className={view === 'installed' ? 'active' : ''} onClick={() => setView('installed')}>已安装 <span>{pluginItems.length}</span></button><button role="tab" aria-selected={view === 'mine'} className={view === 'mine' ? 'active' : ''} onClick={() => setView('mine')}>我的创作 <span>{drafts.length}</span></button></div><div className="plugin-toolbar-meta"><span className={`status-dot ${registry?.registry_ready ? 'success' : 'danger'}`} /><span>{registry?.registry_ready ? '运行正常' : '运行异常'}</span></div></div>
      {view === 'market' ? <section className="plugin-catalog-workspace">
        <aside className="plugin-catalog-browser">
          <div className="plugin-catalog-tools"><label className="plugin-search"><Search size={15} /><span className="sr-only">搜索插件</span><input value={query} onChange={event => setQuery(event.target.value)} placeholder="搜索名称或用途" /></label></div>
          <div className="market-category-block"><div className="market-category-heading"><strong>功能分类</strong><span>按用途查找</span></div><nav className="market-category-nav" aria-label="插件功能分类"><button type="button" className={categoryFilter === 'all' ? 'active' : ''} onClick={() => setCategoryFilter('all')}>全部插件<span>{catalog.length}</span></button>{FUNCTIONAL_CATEGORIES.map(category => <button type="button" key={category.id} className={categoryFilter === category.id ? 'active' : ''} onClick={() => setCategoryFilter(category.id)}>{category.label}<span>{categoryCounts.get(category.id) || 0}</span></button>)}</nav></div>
          <div className="plugin-catalog-result"><span>{filteredCatalog.length} 个结果</span></div>
          <div className="plugin-catalog-list">{visibleCatalog.map(item => { const installed = installedById.get(item.plugin_id); const upgrade = installed?.version ? compareSemanticVersions(item.version, installed.version) > 0 : false; return <button key={item.plugin_id} className={`plugin-catalog-item ${selectedMarket?.plugin_id === item.plugin_id ? 'selected' : ''}`} onClick={() => setSelectedMarketId(item.plugin_id)}><span className="plugin-card-mark">{item.name.slice(0, 1)}</span><span><strong>{item.name}</strong><small>{item.description || catalogSourceLabel(item.source)}</small><small className="catalog-item-author">作者：{item.author_name || '未知作者'}</small></span><span className={`skill-state-label ${upgrade ? 'warn' : installed ? 'success' : 'neutral'}`}>{upgrade ? '可更新' : installed ? '已安装' : catalogAssignmentLabel(item)}</span></button>; })}{visibleCatalog.length < filteredCatalog.length ? <button className="plugin-load-more" onClick={() => setVisibleLimit(current => current + 50)}>加载更多</button> : null}{!visibleCatalog.length ? <EmptyState icon={Search} title="没有匹配的插件" text={catalog.length ? '调整关键词或筛选条件。' : '插件库暂无内容。'} /> : null}</div>
        </aside>
        <main className="plugin-catalog-detail">{selectedMarket ? <MarketPluginDetail item={selectedMarket} installed={installedById.get(selectedMarket.plugin_id)} catalog={catalog} planning={planningId === selectedMarket.plugin_id} onPlan={() => void openInstallPlan(selectedMarket.plugin_id)} /> : <EmptyState icon={Blocks} title="选择一个插件" text="查看功能、依赖和权限。" />}</main>
      </section> : view === 'mine' ? <PluginDraftWorkspace drafts={drafts} selected={selectedDraft} submissions={submissions} busyAction={busyAction} onSelect={setSelectedDraftId} onCreateRevision={onCreateRevision} onTest={onTestDraft} onConfirm={onConfirmDraft} onSubmit={onSubmitDraft} /> : <>
      <div className="plugin-workspace">
        <aside className="plugin-list" aria-label="本机插件列表">
          <div className="plugin-list-header"><strong>已安装</strong><span className="section-count">{pluginItems.length}</span></div>
          <div className="plugin-list-body">
            {pluginItems.map(item => (
              <button key={item.id} type="button" className={`plugin-list-item ${selectedPlugin?.id === item.id ? 'selected' : ''}`} onClick={() => setSelectedId(item.id)}>
                <span className={`status-dot ${item.status === 'failed' ? 'danger' : item.enabled ? 'success' : ''}`} />
                <span><strong>{item.name || item.id}</strong><small>作者：{item.author_name || catalog.find(candidate => candidate.plugin_id === item.id)?.author_name || '未知作者'}</small><small>{item.circuit_open ? '已熔断' : item.id}</small></span>
                <small>v{item.version || '--'}</small>
              </button>
            ))}
            {pluginItems.length === 0 ? <EmptyState icon={Blocks} title="暂无本机插件" text="插件可从 HiMind 工作台下发，也可以安装到这台电脑。" /> : null}
          </div>
        </aside>
      <main className="plugin-detail">
          {selectedPlugin ? <PluginDetail key={selectedPlugin.id} item={selectedPlugin} capabilities={selectedCapabilities} catalog={catalog} onUninstall={onUninstall} onRollback={onRollback} onSetEnabled={onSetEnabled} onUnregisterDevelopment={onUnregisterDevelopment} onInvokeDevelopment={onInvokeDevelopment} onOpenView={onOpenView} onCreateShortcut={onCreateShortcut} /> : <EmptyState icon={Blocks} title="选择插件" text="查看本机运行信息。" />}
        </main>
      </div></>}
      {installPlan || planError ? <PluginInstallPlanDialog plan={installPlan} error={planError} onClose={() => { setInstallPlan(null); setPlanError(''); }} onInstall={() => { if (installPlan) onInstall(installPlan.plugin.plugin_id); setInstallPlan(null); }} /> : null}
    </div>
  );
}

function PluginDraftWorkspace({ drafts, selected, submissions, busyAction, onSelect, onCreateRevision, onTest, onConfirm, onSubmit }: {
  drafts: AuthoringPluginDraft[];
  selected: AuthoringPluginDraft | null;
  submissions: PluginSubmissionStatus[];
  busyAction: string | null;
  onSelect: (id: string) => void;
  onCreateRevision: (pluginId: string, version: string) => void;
  onTest: (pluginId: string, version: string) => void;
  onConfirm: (pluginId: string, version: string) => void;
  onSubmit: (pluginId: string, version: string) => void;
}) {
  if (!drafts.length || !selected) return <div className="plugin-workspace"><EmptyState icon={PackagePlus} title="还没有插件候选" text="AI 完成打包后候选会自动出现在这里，也可以导入本机 .hmpkg。" /></div>;
  const submission = submissions.find(item => item.id === selected.dashboard_submission_id || (item.product_key === selected.manifest.id && item.version === selected.manifest.version));
  const state = pluginDraftState(selected, submission);
  const canRevise = !selected.submitted_at;
  const busy = Boolean(busyAction?.endsWith(selected.manifest.id));
  return <div className="plugin-workspace">
    <aside className="plugin-list" aria-label="插件创作候选">
      <div className="plugin-list-header"><strong>我的创作</strong><span className="section-count">{drafts.length}</span></div>
      <div className="plugin-list-body">{drafts.map(item => { const key = `${item.manifest.id}@${item.manifest.version}`; const itemSubmission = submissions.find(candidate => candidate.id === item.dashboard_submission_id || (candidate.product_key === item.manifest.id && candidate.version === item.manifest.version)); const itemState = pluginDraftState(item, itemSubmission); return <button key={key} type="button" className={`plugin-list-item ${key === `${selected.manifest.id}@${selected.manifest.version}` ? 'selected' : ''}`} onClick={() => onSelect(key)}><span className={`status-dot ${itemState.tone === 'success' ? 'success' : itemState.tone === 'danger' ? 'danger' : ''}`} /><span><strong>{item.manifest.name}</strong><small>{item.manifest.id}</small><small>{itemState.label}</small></span><small>v{item.manifest.version}</small></button>; })}</div>
    </aside>
    <main className="plugin-detail">
      <header className="plugin-detail-header"><div><div className="plugin-title-line"><h3>{selected.manifest.name}</h3><Pill kind={state.tone}>{state.label}</Pill><Pill kind="warn">开发候选</Pill></div><small>作者：{selected.manifest.author || '马宝全'}</small></div><div className="plugin-installed-actions">
        {canRevise ? <button className="btn btn-primary" disabled={busy} onClick={() => onTest(selected.manifest.id, selected.manifest.version)}><Play className={busyAction === `test:${selected.manifest.id}` ? 'spin' : ''} size={15} />部署测试</button> : null}
        {!canRevise ? <button className="btn btn-primary" disabled={busy} onClick={() => onCreateRevision(selected.manifest.id, selected.manifest.version)}><PackagePlus size={15} />创建新版本</button> : null}
        {selected.tested_at && !selected.confirmed_at && canRevise ? <button className="btn" disabled={busy} onClick={() => onConfirm(selected.manifest.id, selected.manifest.version)}><CheckCircle2 size={15} />确认通过</button> : null}
        {selected.confirmed_at && canRevise ? <button className="btn btn-primary" disabled={busy} onClick={() => onSubmit(selected.manifest.id, selected.manifest.version)}><Send size={15} />提交审核</button> : null}
      </div></header>
      <p className="plugin-product-description">{selected.manifest.description || '暂无用途说明。'}</p>
      {submission?.review_note ? <div className="plugin-product-notice"><ShieldCheck size={16} /><div><strong>审核意见</strong><span>{submission.review_note}</span></div></div> : null}
      <div className="skill-authoring-pipeline"><DraftStage complete label="候选已保存" /><DraftStage complete={Boolean(selected.development_path)} label="运行时已加载" /><DraftStage complete={Boolean(selected.tested_at)} label="部署测试完成" /><DraftStage complete={Boolean(selected.confirmed_at)} label="测试已确认" /><DraftStage complete={Boolean(selected.submitted_at)} label="已提交审核" /></div>
      <div className="plugin-meta-grid"><div><span>版本</span><strong>v{selected.manifest.version}</strong></div><div><span>Capability</span><strong>{selected.manifest.capabilities?.length || 0}</strong></div><div><span>测试时间</span><strong>{formatDraftTime(selected.tested_at)}</strong></div><div><span>权限</span><strong>{selected.manifest.permissions?.length || 0}</strong></div></div>
      <section className="plugin-detail-section"><div className="plugin-section-heading"><div><h4>AI 能力</h4></div><span className="section-count">{selected.manifest.capabilities?.length || 0}</span></div><div className="plugin-capability-list">{(selected.manifest.capabilities || []).map(capability => <div key={capability.id}><Bot size={16} /><span><strong>{capability.id}</strong><small>{capability.description || '未提供说明'}</small></span><Pill kind={capability.risk_level === 'read_only' ? 'success' : 'warn'}>{capability.risk_level || '未声明'}</Pill></div>)}</div></section>
      <details className="plugin-technical-panel"><summary>工作区与版本候选</summary><div className="plugin-technical-grid"><div><span>来源</span><strong>{selected.revision_of ? `基于 v${selected.revision_of}` : '本地候选'}</strong></div><div><span>状态</span><strong>{selected.submitted_at ? '版本已冻结' : '工作区可编辑'}</strong></div><div className="wide"><span>工作区</span><code>{selected.workspace_path || selected.development_path || '未关联'}</code></div><div className="wide"><span>SHA-256</span><code>{selected.candidate_sha256}</code></div><div className="wide"><span>候选包</span><code>{selected.candidate_path}</code></div><div className="wide"><span>开发运行目录</span><code>{selected.development_path || '尚未部署测试'}</code></div></div></details>
    </main>
  </div>;
}

function DraftStage({ complete, label }: { complete: boolean; label: string }) { return <div className={complete ? 'complete' : ''}><span>{complete ? <CheckCircle2 size={13} /> : null}</span><strong>{label}</strong></div>; }
function formatDraftTime(value?: string | null) { if (!value) return '--'; const time = Number.parseInt(value, 10); return Number.isFinite(time) ? new Date(time).toLocaleString('zh-CN', { hour12: false }) : value; }
function pluginDraftState(draft: AuthoringPluginDraft, submission?: PluginSubmissionStatus): { label: string; tone: 'success' | 'warn' | 'danger' } { if (submission?.status === 'approved') return { label: '已上架', tone: 'success' }; if (submission?.status === 'changes_requested') return { label: '需修改', tone: 'warn' }; if (submission?.status === 'rejected') return { label: '已拒绝', tone: 'danger' }; if (draft.submitted_at) return { label: '审核中', tone: 'success' }; if (draft.confirmed_at) return { label: '可提交', tone: 'success' }; if (draft.tested_at) return { label: '待确认', tone: 'warn' }; if (draft.development_path) return { label: '开发中', tone: 'warn' }; return { label: '候选', tone: 'warn' }; }

function MarketPluginDetail({ item, installed, catalog, planning, onPlan }: { item: PluginCatalogItem; installed?: PluginItem; catalog: PluginCatalogItem[]; planning: boolean; onPlan: () => void }) {
  const upgrade = installed?.version ? compareSemanticVersions(item.version, installed.version) > 0 : false;
  const current = Boolean(installed) && !upgrade;
  const managed = item.assignment === 'required' && item.management !== 'user_managed';
  const permissions = friendlyPermissions(item.permissions || []);
  return <>
    <header className="plugin-product-header"><div className="plugin-product-title"><span className="plugin-product-mark">{item.name.slice(0, 1)}</span><div><div><h3>{item.name}</h3><Pill kind={item.governance === 'blocked' ? 'danger' : managed ? 'warn' : 'success'}>{catalogAssignmentLabel(item)}</Pill></div><span>{catalogSourceLabel(item.source)} · 作者：{item.author_name || '未知作者'}</span></div></div><button className="btn btn-primary" disabled={current || managed || item.governance === 'blocked' || planning} onClick={onPlan}><Download size={15} />{planning ? '检查依赖' : item.governance === 'blocked' ? '不可使用' : managed ? installed ? '已安装' : '部门内置' : upgrade ? '更新' : current ? '已是最新' : '安装'}</button></header>
    <p className="plugin-product-description">{item.description || '暂无用途说明。'}</p>
    <div className="market-taxonomy"><div><span>功能分类</span><Tags items={functionalCategoryLabels(item.categories)} /></div></div>
    {item.organization_reason ? <div className="plugin-product-notice"><ShieldCheck size={16} /><div><strong>组织说明</strong><span>{item.organization_reason}</span></div></div> : null}
    <div className="plugin-product-facts"><div><AppWindow size={16} /><span>功能页面</span><strong>{item.view_count || 0}</strong></div><div><Bot size={16} /><span>AI 能力</span><strong>{item.capability_ids?.length || 0}</strong></div><div><Blocks size={16} /><span>依赖插件</span><strong>{item.plugin_dependencies?.length || 0}</strong></div><div><ShieldCheck size={16} /><span>访问范围</span><strong>{permissions.length || 0}</strong></div></div>
    <section className="plugin-product-section"><div className="plugin-section-heading"><div><h4>可用功能</h4></div></div><div className="plugin-feature-list"><div><AppWindow size={17} /><span><strong>本机工具</strong><small>{item.view_count ? `${item.view_count} 个可打开的功能页面` : '无独立功能页面'}</small></span></div><div><Bot size={17} /><span><strong>AI 工具能力</strong><small>{item.capability_ids?.length ? `${item.capability_ids.length} 项能力可供 AI 调用` : '不向 AI 提供工具能力'}</small></span></div></div></section>
    <section className="plugin-product-section"><div className="plugin-section-heading"><div><h4>所需插件</h4></div><span className="section-count">{item.plugin_dependencies?.length || 0}</span></div><div className="plugin-product-dependencies">{(item.plugin_dependencies || []).map(dependency => <div key={dependency.plugin_id}><span className="status-dot success" /><PluginDependencyName pluginId={dependency.plugin_id} catalog={catalog} /><span>{dependency.required ? '必需' : '可选'}</span><strong>{dependency.min_version ? `v${dependency.min_version} 以上` : '不限版本'}</strong></div>)}{!item.plugin_dependencies?.length ? <div className="plugin-section-empty">可独立安装，无其他插件依赖</div> : null}</div></section>
    <section className="plugin-product-section"><div className="plugin-section-heading"><div><h4>设备访问</h4></div><span className="section-count">{permissions.length}</span></div><div className="plugin-permission-friendly"><Tags items={permissions} /></div></section>
    <details className="plugin-technical-panel"><summary>版本与技术信息</summary><div className="plugin-technical-grid"><div><span>版本</span><strong>v{item.version}</strong></div><div><span>最低 Agent</span><strong>{item.min_agent_version ? `v${item.min_agent_version}` : '--'}</strong></div><div><span>插件标识</span><code>{item.plugin_id}</code></div><div><span>制品大小</span><strong>{formatFileSize(item.file_size)}</strong></div><div className="wide"><span>SHA-256</span><code>{item.sha256 || '--'}</code></div>{item.capability_ids?.length ? <div className="wide"><span>Capability</span><code>{item.capability_ids.join(' · ')}</code></div> : null}</div>{item.release_notes ? <p>{item.release_notes}</p> : null}</details>
  </>;
}

function PluginDependencyName({ pluginId, catalog }: { pluginId: string; catalog: PluginCatalogItem[] }) {
  const plugin = catalog.find(item => item.plugin_id === pluginId);
  return <span className="plugin-dependency-name"><strong>{plugin?.name || readablePluginID(pluginId)}</strong><code title={pluginId}>{pluginId}</code></span>;
}

function PluginInstallPlanDialog({ plan, error, onClose, onInstall }: { plan: PluginInstallPlan | null; error: string; onClose: () => void; onInstall: () => void }) {
  return <div className="skill-dialog-backdrop"><div className="skill-dialog skill-plan-dialog" role="dialog" aria-modal="true"><div className="skill-dialog-head"><strong>安装插件</strong><button className="btn btn-icon" onClick={onClose} aria-label="关闭"><X size={16} /></button></div>{error ? <div className="skill-dialog-warning">{error}</div> : null}{plan ? <><div className="skill-plan-summary"><strong>{plan.plugin.name} v{plan.plugin.version}</strong><span>{plan.ready ? `需要 ${plan.dependency_actions.filter(item => item.required).length} 个依赖插件` : plan.blocked_reasons.join('；')}</span></div><div className="skill-plan-actions">{plan.dependency_actions.map(action => <div className={`plugin-plan-row ${['blocked', 'unavailable'].includes(action.action) ? 'blocked' : ''}`} key={action.plugin_id}><span className={`status-dot ${action.action === 'satisfied' ? 'success' : ['blocked', 'unavailable'].includes(action.action) ? 'danger' : ''}`} /><span><strong>{action.plugin_name || readablePluginID(action.plugin_id)}</strong><small>{pluginInstallActionLabel(action.action)} · {action.reason}</small><code title={action.plugin_id}>{action.plugin_id}</code></span><strong>{action.target_version ? `v${action.target_version}` : '—'}</strong></div>)}{!plan.dependency_actions.length ? <span className="skill-section-empty">该插件可以独立安装</span> : null}</div></> : null}<div className="skill-dialog-actions"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-primary" disabled={!plan?.ready} onClick={onInstall}><Download size={15} />确认安装</button></div></div></div>;
}

function PluginDetail({ item, capabilities, catalog, onUninstall, onRollback, onSetEnabled, onUnregisterDevelopment, onInvokeDevelopment, onOpenView, onCreateShortcut }: {
  item: PluginItem;
  capabilities: CapabilityItem[];
  catalog: PluginCatalogItem[];
  onUninstall: (pluginId: string) => void;
  onRollback: (pluginId: string) => void;
  onSetEnabled: (pluginId: string, enabled: boolean) => void;
  onUnregisterDevelopment: (pluginId: string) => void;
  onInvokeDevelopment: (pluginId: string, capabilityId: string, input: unknown) => Promise<DevelopmentInvocationResult>;
  onOpenView: (pluginId: string, viewId: string) => void;
  onCreateShortcut: (pluginId: string, viewId: string, title: string) => void;
}) {
  const [debugCapability, setDebugCapability] = useState(item.capabilities?.[0]?.id || '');
  const initialCapability = item.capabilities?.[0];
  const [debugMode, setDebugMode] = useState<'form' | 'json'>(supportsSchemaForm(initialCapability) ? 'form' : 'json');
  const [formInput, setFormInput] = useState<Record<string, unknown>>(() => schemaDefaults(initialCapability?.input_schema));
  const [debugInput, setDebugInput] = useState(() => JSON.stringify(schemaDefaults(initialCapability?.input_schema), null, 2));
  const [debugResult, setDebugResult] = useState<DevelopmentInvocationResult | null>(null);
  const [debugging, setDebugging] = useState(false);
  const [pendingAction, setPendingAction] = useState<{ title: string; description: string; confirmText: string; run: () => void } | null>(null);

  async function runDebug() {
    setDebugging(true);
    try {
      const input = debugMode === 'form' ? formInput : JSON.parse(debugInput || '{}');
      const result = await onInvokeDevelopment(item.id, debugCapability, input);
      setDebugResult(result);
    } catch (error) {
      setDebugResult({ ok: false, duration_ms: 0, error: String(error) });
    } finally {
      setDebugging(false);
    }
  }

  function selectCapability(capabilityId: string) {
    const capability = item.capabilities?.find(candidate => candidate.id === capabilityId);
    const defaults = schemaDefaults(capability?.input_schema);
    setDebugCapability(capabilityId);
    setFormInput(defaults);
    setDebugInput(JSON.stringify(defaults, null, 2));
    setDebugMode(supportsSchemaForm(capability) ? 'form' : 'json');
    setDebugResult(null);
  }

  const selectedCapability = item.capabilities?.find(capability => capability.id === debugCapability);
  const supportsForm = supportsSchemaForm(selectedCapability);
  const friendlyPermissionItems = friendlyPermissions(item.permissions || []);

  return (
    <>
      <div className="plugin-detail-header">
        <div><div className="plugin-title-line"><h3>{item.name || item.id}</h3><Pill kind={item.circuit_open ? 'danger' : item.status === 'installed' && item.enabled ? 'success' : item.status === 'failed' ? 'danger' : 'warn'}>{item.circuit_open ? '已熔断' : item.enabled ? item.status === 'installed' ? '运行中' : item.status : '已停用'}</Pill>{item.failure_count ? <Pill kind="danger">失败 {item.failure_count} 次</Pill> : null}{item.development ? <Pill kind="warn">开发中</Pill> : null}{item.governance ? <Pill kind={item.governance === 'required' || item.governance === 'managed' ? 'warn' : 'success'}>{governanceLabels[item.governance] || item.governance}</Pill> : null}</div></div>
        <div className="plugin-installed-actions">{item.development ? <button className="btn btn-danger-quiet" onClick={() => setPendingAction({ title: '移除开发插件注册？', description: `移除后将停止加载“${item.name || item.id}”的本地工程，但不会删除工程文件。`, confirmText: '确认移除', run: () => onUnregisterDevelopment(item.id) })}><Unplug size={15} />移除开发注册</button> : <><label className="plugin-enable-control"><span>启用</span><span className="toggle"><input type="checkbox" checked={Boolean(item.enabled)} disabled={item.governance === 'required' || item.governance === 'managed'} onChange={event => event.target.checked ? onSetEnabled(item.id, true) : setPendingAction({ title: '确认停用插件？', description: `停用后，Agent 将不再调用“${item.name || item.id}”提供的能力。`, confirmText: '确认停用', run: () => onSetEnabled(item.id, false) })} /><span className="slider" /></span></label><details className="plugin-more-actions"><summary title="更多操作" aria-label="更多操作"><MoreHorizontal size={17} /></summary><div>{item.governance !== 'required' && item.governance !== 'managed' && item.governance !== 'blocked' ? <button disabled={!item.rollback_available} onClick={() => setPendingAction({ title: '确认回滚插件？', description: `将“${item.name || item.id}”切换到 v${item.previous_version || '--'}，当前版本会停止运行。`, confirmText: '确认回滚', run: () => onRollback(item.id) })}>回滚{item.previous_version ? `到 v${item.previous_version}` : ''}</button> : null}<button className="danger-text" disabled={item.governance === 'required' || item.governance === 'managed'} onClick={() => setPendingAction({ title: '确认卸载插件？', description: `卸载后将移除“${item.name || item.id}”，其能力和功能页面会立即不可用。`, confirmText: '确认卸载', run: () => onUninstall(item.id) })}>卸载</button></div></details></>}</div>
      </div>
      {item.error ? <div className="plugin-local-error">{item.error}</div> : null}
      <p className="plugin-product-description">{item.description || '暂无用途说明。'}</p>
      <div className="plugin-meta-grid">
        <div><span>作者</span><strong>{item.author_name || catalog.find(candidate => candidate.plugin_id === item.id)?.author_name || '未知作者'}</strong></div>
        <div><span>版本</span><strong>{item.version || '--'}</strong></div>
        <div><span>{item.development ? '入口构建时间' : '功能页面'}</span><strong>{item.development ? formatBuildTime(item.entry_modified_at) : item.views?.length || 0}</strong></div>
        <div><span>{item.development ? '入口大小' : 'AI 能力'}</span><strong>{item.development ? formatFileSize(item.entry_size) : capabilities.length}</strong></div>
        <div><span>依赖插件</span><strong>{item.plugin_dependencies?.length || 0}</strong></div>
      </div>
      <section className="plugin-detail-section">
        <div className="plugin-section-heading"><div><h4>功能页面</h4><p>在独立窗口运行当前插件的本机功能。</p></div><span className="section-count">{item.views?.length || 0}</span></div>
        <div className="plugin-view-list">
          {item.views?.length ? item.views.map(view => (
            <div className="plugin-view-row" key={view.id}>
              <MonitorUp size={17} />
              <div><strong>{view.title}</strong><code>{view.id}</code></div>
              <div className="actions-row"><button className="btn btn-primary" onClick={() => onOpenView(item.id, view.id)}><ExternalLink size={15} />打开</button><button className="btn" onClick={() => onCreateShortcut(item.id, view.id, view.title)}>创建快捷方式</button></div>
            </div>
          )) : <div className="plugin-section-empty">此插件没有本机功能页面</div>}
        </div>
      </section>
      <section className="plugin-detail-section">
        <div className="plugin-section-heading"><div><h4>所需插件</h4></div><span className="section-count">{item.plugin_dependencies?.length || 0}</span></div>
        <div className="plugin-product-dependencies">{(item.plugin_dependencies || []).map(dependency => <div key={dependency.plugin_id}><span className="status-dot success" /><PluginDependencyName pluginId={dependency.plugin_id} catalog={catalog} /><span>{dependency.required ? '必需' : '可选'}</span><strong>{dependency.min_version ? `v${dependency.min_version} 以上` : '不限版本'}</strong></div>)}{!item.plugin_dependencies?.length ? <div className="plugin-section-empty">可独立运行，无其他插件依赖</div> : null}</div>
      </section>
      <section className="plugin-detail-section">
        <div className="plugin-section-heading"><div><h4>设备访问</h4></div><span className="section-count">{friendlyPermissionItems.length}</span></div>
        <div className="plugin-permission-list"><Tags items={friendlyPermissionItems} /></div>
      </section>
      {!item.development ? <details className="plugin-technical-panel"><summary>技术信息</summary><div className="plugin-technical-grid"><div><span>插件标识</span><code>{item.id}</code></div><div><span>运行时</span><strong>{item.runtime || '--'}</strong></div><div><span>最低 Agent</span><strong>{item.min_agent_version ? `v${item.min_agent_version}` : '--'}</strong></div><div><span>上一版本</span><strong>{item.previous_version ? `v${item.previous_version}` : '--'}</strong></div>{item.permissions?.length ? <div className="wide"><span>权限标识</span><code>{item.permissions.join(' · ')}</code></div> : null}</div>{capabilities.length ? <div className="plugin-capability-list">{capabilities.map(capability => <div className="plugin-capability-row" key={capability.id}><code>{capability.id}</code><span>{riskLevelLabel(capability.risk_level)}</span><p>{capability.description || '--'}</p></div>)}</div> : null}</details> : null}
      {item.development ? <section className="plugin-detail-section plugin-capability-section">
        <div className="plugin-section-heading"><div><h4>Capability 调试</h4><p>修改 JSON 参数后直接调用当前本地工程的编译入口。</p></div><FlaskConical size={17} /></div>
        <div className="plugin-debug-grid">
          <label><span>Capability</span><select value={debugCapability} onChange={event => selectCapability(event.target.value)}>{item.capabilities?.map(capability => <option key={capability.id} value={capability.id}>{capability.id}</option>)}</select></label>
          <div className="plugin-debug-modes"><button className={debugMode === 'form' ? 'active' : ''} disabled={!supportsForm} onClick={() => setDebugMode('form')}>表单</button><button className={debugMode === 'json' ? 'active' : ''} onClick={() => { setDebugInput(JSON.stringify(formInput, null, 2)); setDebugMode('json'); }}>JSON</button></div>
          {debugMode === 'form' && selectedCapability ? <SchemaForm schema={selectedCapability.input_schema} value={formInput} onChange={setFormInput} /> : <label className="plugin-debug-json"><span>JSON 参数</span><textarea value={debugInput} onChange={event => setDebugInput(event.target.value)} rows={7} spellCheck={false} /></label>}
          <button className="btn btn-primary" disabled={!debugCapability || debugging} onClick={runDebug}><Play size={15} />{debugging ? '运行中...' : '运行调试'}</button>
          <div className={`plugin-debug-result ${debugResult ? debugResult.ok ? 'success' : 'danger' : ''}`}><div><span>运行结果</span>{debugResult ? <strong>{debugResult.ok ? '成功' : '失败'} · {debugResult.duration_ms} ms</strong> : null}</div><pre>{debugResult ? debugResult.ok ? JSON.stringify(debugResult.result, null, 2) : debugResult.error : '尚未运行'}</pre></div>
        </div>
      </section> : null}
      {pendingAction ? <ConfirmPluginAction action={pendingAction} onClose={() => setPendingAction(null)} /> : null}
    </>
  );
}

function ConfirmPluginAction({ action, onClose }: { action: { title: string; description: string; confirmText: string; run: () => void }; onClose: () => void }) {
  return <div className="modal-backdrop" role="presentation"><div className="modal" role="dialog" aria-modal="true" aria-labelledby="plugin-action-title"><div className="modal-header"><div><h3 id="plugin-action-title">{action.title}</h3><p>{action.description}</p></div><button className="btn btn-icon" aria-label="关闭" onClick={onClose}><X size={16} /></button></div><div className="modal-body"><div className="modal-actions"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-danger" onClick={() => { action.run(); onClose(); }}>{action.confirmText}</button></div></div></div></div>;
}

function supportsSchemaForm(capability?: PluginCapability) {
  const schema = capability?.input_schema;
  if (!schema || schema.type !== 'object' || !schema.properties) return false;
  return Object.values(schema.properties).every(property => ['string', 'number', 'integer', 'boolean'].includes(property.type || ''));
}

function schemaDefaults(schema?: PluginJsonSchema) {
  const result: Record<string, unknown> = {};
  for (const [name, property] of Object.entries(schema?.properties || {})) {
    if (property.default !== undefined) result[name] = property.default;
    else if (property.type === 'boolean') result[name] = false;
    else if (property.type === 'number' || property.type === 'integer') result[name] = property.minimum ?? 0;
    else result[name] = '';
  }
  return result;
}

function SchemaForm({ schema, value, onChange }: { schema?: PluginJsonSchema; value: Record<string, unknown>; onChange: (value: Record<string, unknown>) => void }) {
  const required = new Set(schema?.required || []);
  return <div className="plugin-schema-form">{Object.entries(schema?.properties || {}).map(([name, property]) => <label key={name}><span>{name}{required.has(name) ? ' *' : ''}</span>{property.type === 'boolean' ? <input type="checkbox" checked={Boolean(value[name])} onChange={event => onChange({ ...value, [name]: event.target.checked })} /> : <input type={property.type === 'number' || property.type === 'integer' ? 'number' : 'text'} min={property.minimum} step={property.type === 'integer' ? 1 : 'any'} value={String(value[name] ?? '')} onChange={event => onChange({ ...value, [name]: property.type === 'number' || property.type === 'integer' ? event.target.value === '' ? '' : Number(event.target.value) : event.target.value })} />}{property.description ? <small>{property.description}</small> : null}</label>)}</div>;
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

function catalogSourceLabel(source?: string) {
  if (source === 'system') return '系统内置';
  if (source === 'organization') return '组织提供';
  return '公共插件库';
}

function catalogAssignmentLabel(item: PluginCatalogItem) {
  if (item.assignment === 'blocked' || item.governance === 'blocked') return '组织已禁止';
  if (item.source === 'system') return '系统内置';
  if (item.assignment === 'required') return '部门内置';
  if (item.assignment === 'recommended') return '组织推荐';
  return '可安装';
}

function readablePluginID(value: string) { const tail = value.split('.').filter(Boolean).pop() || value; return tail.split(/[-_]/).filter(Boolean).map(part => part.charAt(0).toUpperCase() + part.slice(1)).join(' '); }
function pluginInstallActionLabel(action: string) { return ({ satisfied: '已满足', install: '将安装', update: '将升级', blocked: '已阻止', unavailable: '不可用' } as Record<string, string>)[action] || action; }

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
