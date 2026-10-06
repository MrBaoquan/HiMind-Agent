import { useEffect, useMemo, useState } from 'react';
import { AppWindow, ArrowLeft, Blocks, Bot, Download, ExternalLink, FolderOpen, MonitorUp, Plus, RefreshCw, Search, ShieldCheck, Store, Trash2, X } from 'lucide-react';
import type { CapabilityItem, ExtensionDesiredItem, ExtensionDesiredState, PluginCatalogItem, PluginInstallPlan, PluginItem, PluginRegistry } from '../services/agentApi';
import { BusyIndicator } from '../components/BusyIndicator';
import { EmptyState, Pill, Tags } from '../components/Common';
import { ActionMenu, ActionMenuItem } from '../components/ActionMenu';
import { ExtensionKindMark } from '../components/ExtensionKindMark';
import { ExtensionBatchUpdateDialog } from '../components/ExtensionBatchUpdateDialog';
import { OperationPlanCard } from '../components/OperationPlanCard';
import { compareSemanticVersions, installActionLabel } from './marketCatalog';
import { pluginFailureState, pluginNeedsRepair } from './pluginHealth';

const governanceLabels: Record<string, string> = {
  required: '系统内置',
  managed: '组织管理',
  blocked: '不可安装',
};

/// 「可选」是插件的默认治理状态，不是一条信息：这一页看到的本来就是自己装的插件，
/// 每行都挂一个「可选插件」只会稀释真正的状态标签（运行异常 / 已停用 / 组织管理）。
function governanceBadge(item: PluginItem): { label: string; tone: 'warn' | 'danger' | 'success' } | null {
  const governance = item.governance || '';
  if (governance === 'optional' || !governance) return null;
  const label = governanceLabels[governance] || governance;
  return { label, tone: governance === 'blocked' ? 'danger' : 'warn' };
}

/**
 * 「我的能力 → 插件」看到的就是这批：自己装的插件；对接工作台后，组织管理的插件
 * 只在「组织管理」页签里出现（它们不能手动装、手动停）。
 * 计数和列表必须同源，所以这段筛选对外导出给容器页用。
 */
export function userInstalledPlugins(pluginItems: PluginItem[], desired: ExtensionDesiredState | null, dashboardEnabled: boolean) {
  return dashboardEnabled
    ? pluginItems.filter(item => !isManagedPlugin(item, desired))
    : pluginItems.filter(item => item.availability !== 'control_plane');
}

export function PluginsPage({ loading, registry, catalog, desired, capabilities, dashboardEnabled, catalogEnabled, onLoadVersions, onPlanInstall, onInstall, onUninstall, onRollback, onRepair, onSetEnabled, onOpenView, onCreateShortcut, onImportLocal, onImportGithub, onOpenExtensions, onBatchUpdateFinished }: {
  loading: boolean;
  registry: PluginRegistry | null;
  catalog: PluginCatalogItem[];
  desired: ExtensionDesiredState | null;
  capabilities: CapabilityItem[];
  dashboardEnabled: boolean;
  catalogEnabled: boolean;
  onLoadVersions: (pluginId: string) => Promise<PluginCatalogItem[]>;
  onPlanInstall: (pluginId: string, version?: string) => Promise<PluginInstallPlan>;
  onInstall: (pluginId: string, version?: string) => void;
  onUninstall: (pluginId: string) => void;
  onRollback: (pluginId: string) => void;
  onRepair: (pluginId: string) => void;
  onSetEnabled: (pluginId: string, enabled: boolean) => void;
  onOpenView: (pluginId: string, viewId: string) => void;
  onCreateShortcut: (pluginId: string, viewId: string, title: string) => void;
  onImportLocal: () => void;
  onImportGithub: (sourceUrl: string) => Promise<void>;
  onOpenExtensions: () => void;
  /// 批量更新动过版本之后，插件清单与组织期望状态都得重新取一遍。
  onBatchUpdateFinished: () => void | Promise<void>;
}) {
  const pluginItems = registry?.items || [];
  const userPluginItems = useMemo(() => userInstalledPlugins(pluginItems, desired, dashboardEnabled), [dashboardEnabled, desired, pluginItems]);
  const [selectedId, setSelectedId] = useState('');
  const [detailOpen, setDetailOpen] = useState(false);
  const [installPlan, setInstallPlan] = useState<PluginInstallPlan | null>(null);
  const [planError, setPlanError] = useState('');
  const [planningId, setPlanningId] = useState('');
  const [query, setQuery] = useState('');
  const [updatesOnly, setUpdatesOnly] = useState(false);
  const [batchOpen, setBatchOpen] = useState(false);
  const [githubOpen, setGithubOpen] = useState(false);
  const [githubSourceUrl, setGithubSourceUrl] = useState('');
  const [githubBusy, setGithubBusy] = useState(false);
  const [githubError, setGithubError] = useState('');
  // 目录里的每个插件只留最高版本：列表的「可更新」判断和市场页用同一份口径。
  const latestVersionById = useMemo(() => {
    const versions = new Map<string, string>();
    for (const item of catalog) {
      const current = versions.get(item.plugin_id);
      if (!current || compareSemanticVersions(item.version, current) > 0) versions.set(item.plugin_id, item.version);
    }
    return versions;
  }, [catalog]);
  // 开发中的插件由扩展开发自己管版本，组织管理的插件不能手动装，都不算"可更新"。
  const pluginUpdatable = (item: PluginItem) => {
    if (item.development || item.governance === 'required' || item.governance === 'managed') return false;
    const latest = latestVersionById.get(item.id);
    return Boolean(latest && item.version && compareSemanticVersions(latest, item.version) > 0);
  };
  const updatableCount = userPluginItems.filter(pluginUpdatable).length;
  const filteredPlugins = useMemo(() => {
    const keyword = query.trim().toLowerCase();
    return userPluginItems.filter(item => {
      if (updatesOnly && !pluginUpdatable(item)) return false;
      if (!keyword) return true;
      return [item.id, item.name, item.description, item.author_name].filter(Boolean).join(' ').toLowerCase().includes(keyword);
    });
  }, [latestVersionById, query, updatesOnly, userPluginItems]);
  // 详情跟着当前列表走：搜索或「只看可更新」把选中的插件筛掉时，右侧不能还停在
  // 一个列表里已经看不见的插件上，否则用户会对着 A 的详情去做 B 的操作。
  const selectedPlugin = useMemo(
    () => filteredPlugins.find(item => item.id === selectedId) || filteredPlugins[0] || null,
    [filteredPlugins, selectedId],
  );
  const selectedCapabilities = useMemo(
    () => selectedPlugin ? capabilities.filter(item => item.source === `plugin:${selectedPlugin.id}` || selectedPlugin.capabilities?.some(capability => capability.id === item.id)) : [],
    [capabilities, selectedPlugin],
  );
  const installedById = new Map(pluginItems.map(item => [item.id, item]));

  async function openInstallPlan(pluginId: string, version?: string) {
    setPlanningId(pluginId);
    setPlanError('');
    try { setInstallPlan(await onPlanInstall(pluginId, version)); }
    catch { setPlanError('暂时无法检查插件，请稍后重试。'); }
    finally { setPlanningId(''); }
  }

  if (loading && !registry && catalog.length === 0) return <div className="page-loading"><BusyIndicator size={15} />正在加载插件</div>;

  return (
    <div className="plugin-page">
      {/* 页面标题归「我的能力」容器，这里只出这个类型自己的动作区。 */}
      <div className="plugin-toolbar"><div className="actions-row">
        <ActionMenu label="安装插件" icon={<Plus size={15} />} title="安装插件" panelWidth={208}>{close => <>
          <ActionMenuItem icon={<Store size={15} />} label="获取更多工具" onClick={() => { onOpenExtensions(); close(); }} />
          <div className="app-menu-separator" role="separator" />
          <ActionMenuItem icon={<ExternalLink size={15} />} label="从 GitHub 导入" onClick={() => { setGithubError(''); setGithubOpen(true); close(); }} />
          <ActionMenuItem icon={<FolderOpen size={15} />} label="从本地导入" onClick={() => { onImportLocal(); close(); }} />
        </>}</ActionMenu>
      </div><div className="plugin-toolbar-meta">{updatableCount ? <button type="button" className={`market-update-chip${updatesOnly ? ' active' : ''}`} title="只显示有可用更新的插件" onClick={() => setUpdatesOnly(current => !current)}>{updatableCount} 个可更新</button> : null}{updatableCount ? <button type="button" className="market-update-action" onClick={() => setBatchOpen(true)} title="核对来源后批量更新扩展"><Download size={12} />全部更新</button> : null}<span className={`status-dot ${registry?.registry_ready ? 'success' : 'danger'}`} /><span>{registry?.registry_ready ? '插件可用' : '插件需要处理'}</span></div></div>
      <div className={`plugin-workspace compact-master-detail ${detailOpen ? 'detail-open' : ''}`}>
        <aside className="plugin-list plugin-list-with-search" aria-label="已安装插件列表">
          <div className="plugin-list-header"><strong>已安装</strong><span className="section-count">{userPluginItems.length}</span></div>
          <div className="plugin-list-search"><label className="skill-search"><Search size={15} /><input value={query} onChange={event => setQuery(event.target.value)} placeholder="搜索插件" /></label></div>
          <div className="plugin-list-body">
            {filteredPlugins.map(item => {
              // 默认的「已启用」由左侧状态点表达，不再单独占一行；只有需要留意的状态才补文字。
              const stateText = pluginListStateText(item);
              return (
              <button key={item.id} type="button" className={`plugin-list-item ${selectedPlugin?.id === item.id ? 'selected' : ''}`} onClick={() => { setSelectedId(item.id); setDetailOpen(true); }}>
                <span className={`status-dot ${pluginListDot(item)}`} title={stateText || '已启用'} />
                <span><strong>{item.name || item.id}</strong><small>作者：{item.author_name || catalog.find(candidate => candidate.plugin_id === item.id)?.author_name || '未知作者'}{stateText ? ` · ${stateText}` : ''}</small></span>
                <span className="plugin-list-state">{pluginUpdatable(item) ? <span className="skill-state-label warn">可更新</span> : null}<small>v{item.version || '--'}</small></span>
              </button>
              );
            })}
          {!userPluginItems.length ? <EmptyState icon={Blocks} title="还没有安装插件" text="到「市场」浏览安装，或用上面的「安装插件」导入本机、GitHub 插件。" /> : null}
{userPluginItems.length > 0 && !filteredPlugins.length ? <EmptyState icon={Search} title={updatesOnly ? '已安装的插件都是最新版' : '没有匹配的插件'} text={updatesOnly ? '可以关掉「可更新」筛选查看全部。' : '换个关键词试试。'} /> : null}
          </div>
        </aside>
      <main className="plugin-detail">
          <button className="workspace-back" onClick={() => setDetailOpen(false)}><ArrowLeft size={15} />返回插件列表</button>
           {selectedPlugin ? <PluginDetail key={selectedPlugin.id} item={selectedPlugin} capabilities={selectedCapabilities} catalog={catalog} catalogEnabled={catalogEnabled} onLoadVersions={onLoadVersions} onPlanVersion={(version) => void openInstallPlan(selectedPlugin.id, version)} onUninstall={onUninstall} onRollback={onRollback} onRepair={onRepair} onSetEnabled={onSetEnabled} onOpenView={onOpenView} onCreateShortcut={onCreateShortcut} /> : <EmptyState icon={Blocks} title="选择插件" text="查看功能、依赖和版本。" />}
        </main>
       </div>
      {installPlan || planError ? <PluginInstallPlanDialog plan={installPlan} error={planError} currentVersion={installPlan ? installedById.get(installPlan.plugin.plugin_id)?.version : undefined} onClose={() => { setInstallPlan(null); setPlanError(''); }} onInstall={() => { if (installPlan) onInstall(installPlan.plugin.plugin_id, installPlan.plugin.version); setInstallPlan(null); }} /> : null}
      {githubOpen ? <div className="modal-backdrop" role="presentation"><div className="modal" role="dialog" aria-modal="true" aria-labelledby="github-plugin-title"><div className="modal-header"><div><h3 id="github-plugin-title">从 GitHub 导入插件</h3><p>粘贴仓库链接即可；子目录和版本可按 UPM 方式写在链接中。</p></div><button className="btn btn-icon" aria-label="关闭" title="关闭" onClick={() => setGithubOpen(false)}><X size={16} /></button></div><div className="modal-body"><div className="field-group"><label className="field-label" htmlFor="github-plugin-source-url">GitHub 链接</label><input id="github-plugin-source-url" value={githubSourceUrl} onChange={event => setGithubSourceUrl(event.target.value)} placeholder="https://github.com/owner/repository.git?path=/plugins/example#v1.0.0" /></div>{githubError ? <div className="inline-feedback visible" role="status">{githubError}</div> : null}<div className="modal-actions"><span /><div className="actions-row"><button className="btn" onClick={() => setGithubOpen(false)}>取消</button><button className="btn btn-primary" disabled={githubBusy || !githubSourceUrl.trim()} onClick={async () => { setGithubBusy(true); setGithubError(''); try { await onImportGithub(githubSourceUrl.trim()); setGithubOpen(false); } catch (error) { setGithubError(error instanceof Error ? error.message : 'GitHub 插件导入失败'); } finally { setGithubBusy(false); } }}>{githubBusy ? '导入中...' : '导入插件'}</button></div></div></div></div></div> : null}
      <ExtensionBatchUpdateDialog open={batchOpen} onClose={() => setBatchOpen(false)} onFinished={onBatchUpdateFinished} />
    </div>
  );
}

function DetailTabs({ value, onChange }: { value: 'details' | 'versions'; onChange: (value: 'details' | 'versions') => void }) {
  return <div className="extension-detail-tabs" role="tablist"><button role="tab" aria-selected={value === 'details'} className={value === 'details' ? 'active' : ''} onClick={() => onChange('details')}>详情</button><button role="tab" aria-selected={value === 'versions'} className={value === 'versions' ? 'active' : ''} onClick={() => onChange('versions')}>版本</button></div>;
}

function PluginVersionList({ versions, currentVersion, lockedLabel, loading, error, onSelect }: { versions: PluginCatalogItem[]; currentVersion?: string; lockedLabel?: string; loading: boolean; error: string; onSelect: (version: string) => void }) {
  const sorted = [...versions].sort((left, right) => compareSemanticVersions(right.version, left.version));
  // 本机已经是这一版的行只留状态标签：再挂一个禁用按钮只是把「已安装」写两遍。
  // 安装与升级用主按钮，降级退回普通样式；重新安装走来源管理。
  return <section className="extension-version-list">{loading ? <div className="extension-version-empty"><BusyIndicator size={15} />正在读取版本</div> : null}{error ? <div className="plugin-local-error">{error}</div> : null}{!loading && sorted.map(version => { const installed = currentVersion === version.version; const action = installActionLabel({ target: version.version, installed: currentVersion, locked: lockedLabel }); return <article className="extension-version-row" key={version.version}><div className="extension-version-main"><div><strong>v{version.version}</strong>{installed ? <Pill kind="success">已安装</Pill> : null}</div><time>{formatPublishedAt(version.published_at)}</time>{version.release_notes ? <p>{version.release_notes}</p> : null}</div>{installed ? null : <button className={compareSemanticVersions(version.version, currentVersion || '') > 0 ? 'btn btn-primary' : 'btn'} disabled={Boolean(lockedLabel)} onClick={() => onSelect(version.version)}>{action}</button>}</article>; })}</section>;
}

function PluginDependencyName({ pluginId, catalog }: { pluginId: string; catalog: PluginCatalogItem[] }) {
  const plugin = catalog.find(item => item.plugin_id === pluginId);
  return <span className="plugin-dependency-name"><strong>{plugin?.name || readablePluginID(pluginId)}</strong></span>;
}

function PluginInstallPlanDialog({ plan, error, currentVersion, onClose, onInstall }: { plan: PluginInstallPlan | null; error: string; currentVersion?: string; onClose: () => void; onInstall: () => void }) {
  // 动词与列表按钮、市场保持一套：安装 / 更新 / 重新安装 / 降级。
  const diff = plan && currentVersion ? compareSemanticVersions(plan.plugin.version, currentVersion) : 0;
  const action = !plan || !currentVersion ? '安装' : diff > 0 ? '更新' : diff === 0 ? '重新安装' : '降级';
  const title = `${action}插件`;
  return <div className="skill-dialog-backdrop"><div className="skill-dialog skill-plan-dialog" role="dialog" aria-modal="true"><div className="skill-dialog-head"><strong>{title}</strong><button className="btn btn-icon" onClick={onClose} aria-label="关闭"><X size={16} /></button></div>{error ? <div className="skill-dialog-warning">{error}</div> : null}{plan ? <><div className="skill-plan-summary"><strong>{plan.plugin.name} v{plan.plugin.version}</strong><span>{plan.ready ? '可以安装' : '当前无法安装'}</span></div>{plan.plan ? <OperationPlanCard plan={plan.plan} heading="这次会做什么" showDependencies={false} /> : null}<div className="skill-plan-actions">{plan.dependency_actions.map(action => <div className={`plugin-plan-row ${['blocked', 'unavailable'].includes(action.action) ? 'blocked' : ''}`} key={action.plugin_id}><span className={`status-dot ${action.action === 'satisfied' ? 'success' : ['blocked', 'unavailable'].includes(action.action) ? 'danger' : ''}`} /><span><strong>{action.plugin_name || readablePluginID(action.plugin_id)}</strong><small>{pluginInstallActionDescription(action.action)}</small></span><strong>{action.target_version ? `v${action.target_version}` : '—'}</strong></div>)}{!plan.dependency_actions.length ? <span className="skill-section-empty">无依赖</span> : null}</div></> : null}<div className="skill-dialog-actions"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-primary" disabled={!plan?.ready} onClick={onInstall}><Download size={15} />确认{action}</button></div></div></div>;
}

function PluginDetail({ item, capabilities, catalog, catalogEnabled, onLoadVersions, onPlanVersion, onUninstall, onRollback, onRepair, onSetEnabled, onOpenView, onCreateShortcut }: {
  item: PluginItem;
  capabilities: CapabilityItem[];
  catalog: PluginCatalogItem[];
  catalogEnabled: boolean;
  onLoadVersions: (pluginId: string) => Promise<PluginCatalogItem[]>;
  onPlanVersion: (version: string) => void;
  onUninstall: (pluginId: string) => void;
  onRollback: (pluginId: string) => void;
  onRepair: (pluginId: string) => void;
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
  // 列表里的「可更新」和这里的「更新到 vX」必须同源：都取目录里的最高版本。
  const latestVersion = catalog.filter(candidate => candidate.plugin_id === item.id)
    .reduce((best, candidate) => !best || compareSemanticVersions(candidate.version, best) > 0 ? candidate.version : best, '');
  const updatable = !item.development && !managed && Boolean(latestVersion && item.version && compareSemanticVersions(latestVersion, item.version) > 0);
  // 只有「现在还在影响调用」的失败才给修复入口，判断口径和「组织管理」页共用一份。
  // 开发中的插件同样给：它们的健康记录写在扩展开发目录里，修复会把两处一起清掉，
  // 而失败提示里既已写明「点修复并重试」，就不能只在开发中插件上留一句空话。
  const needsRepair = pluginNeedsRepair(item);

  useEffect(() => {
    if (!catalogEnabled && tab !== 'details') setTab('details');
  }, [catalogEnabled, tab]);

  useEffect(() => {
    if (!catalogEnabled || item.development || tab !== 'versions') return;
    let active = true;
    setVersionsLoading(true);
    setVersionsError('');
    onLoadVersions(item.id).then(result => { if (active) setVersions(result); }).catch(() => { if (active) setVersionsError('暂时无法读取版本，请稍后重试。'); }).finally(() => { if (active) setVersionsLoading(false); });
    return () => { active = false; };
  }, [catalogEnabled, item.development, item.id, onLoadVersions, tab]);

  const governanceTag = governanceBadge(item);
  return (
    <>
      <div className="plugin-detail-header">
        <div className="plugin-detail-title">
          <ExtensionKindMark kind="plugin" />
          <div className="plugin-title-line"><h3>{item.name || item.id}</h3><Pill kind={item.circuit_open ? 'danger' : item.status === 'installed' && item.enabled ? 'success' : item.status === 'failed' ? 'danger' : 'warn'}>{item.circuit_open || item.status === 'failed' ? '运行异常' : item.enabled ? '已安装' : '已停用'}</Pill>{item.development ? <Pill kind="warn">开发中</Pill> : null}{governanceTag ? <Pill kind={governanceTag.tone}>{governanceTag.label}</Pill> : null}</div>
        </div>
        {/* 回滚暂时不暴露入口（后端与 registry 里的 previous 版本都保留），详情级只留更新、启用与卸载。 */}
        <div className="plugin-installed-actions">
          {needsRepair ? <button className="btn btn-primary" type="button" title="清除失败记录并立即重新尝试调用" onClick={() => onRepair(item.id)}><RefreshCw size={15} />修复并重试</button> : null}
          {updatable ? <button className={`btn${needsRepair ? '' : ' btn-primary'}`} type="button" onClick={() => onPlanVersion(latestVersion)}><Download size={15} />更新到 v{latestVersion}</button> : null}
          {!item.development ? <>
            {/* 类名从 plugin- 前缀改成通用名：工作流详情头部用的是同一个控件，
                两个页面共用一条规则，改一处两边一起变。 */}
            <label className="enable-control"><span>启用</span><span className="toggle"><input type="checkbox" checked={Boolean(item.enabled)} disabled={item.governance === 'required' || item.governance === 'managed'} onChange={event => event.target.checked ? onSetEnabled(item.id, true) : setPendingAction({ title: '确认停用插件？', description: `停用后，Agent 将不再调用“${item.name || item.id}”提供的能力。`, confirmText: '确认停用', run: () => onSetEnabled(item.id, false) })} /><span className="slider" /></span></label>
            <button className="btn btn-danger-quiet" type="button" disabled={item.governance === 'required' || item.governance === 'managed'} onClick={() => setPendingAction({ title: '确认卸载插件？', description: `卸载后将移除“${item.name || item.id}”，其能力和功能页面会立即不可用。`, confirmText: '确认卸载', run: () => onUninstall(item.id) })}><Trash2 size={15} />卸载</button>
          </> : null}
        </div>
      </div>
      {item.error ? <div className="plugin-local-error">
        {pluginFailureHeadline(item)}
        <small>{pluginFailureHint(item)}</small>
      </div> : null}
      {/* 开发登记接管已安装副本时必须两个版本都说明白：只说“本机开发版本”，
          用户会以为插件被降级或者装了个别的版本。 */}
      {item.development ? <div className="plugin-product-notice"><ShieldCheck size={16} /><div>{item.overrides_installed_version
        ? <><strong>本机开发版本生效中</strong><span>已安装 v{item.overrides_installed_version}，当前运行的是本机开发版本 v{item.version || '--'}。构建、测试和移除统一在“扩展开发”中管理。</span></>
                    : <><strong>本机开发版本</strong><span>本机未装副本，直接运行扩展开发目录里的构建产物；构建、测试和移除统一在「扩展开发」中管理。</span></>}</div></div> : null}
      {item.description ? <p className="plugin-product-description">{item.description}</p> : null}
       {!item.development && catalogEnabled ? <DetailTabs value={tab} onChange={setTab} /> : null}
       {!item.development && catalogEnabled && tab === 'versions' ? <PluginVersionList versions={versions.length ? versions : catalogItem ? [catalogItem] : []} currentVersion={item.version} lockedLabel={item.governance === 'blocked' ? '不可安装' : managed ? '组织管理' : undefined} loading={versionsLoading} error={versionsError} onSelect={onPlanVersion} /> : <>
        {item.development ? <div className="plugin-meta-grid"><div><span>当前运行</span><strong>v{item.version || '--'}</strong></div>{item.overrides_installed_version ? <div><span>已安装</span><strong>v{item.overrides_installed_version}</strong></div> : null}<div><span>构建时间</span><strong>{formatBuildTime(item.entry_modified_at)}</strong></div><div><span>入口大小</span><strong>{formatFileSize(item.entry_size)}</strong></div></div> : null}
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
        {!item.development ? <details className="plugin-technical-panel"><summary>开发者信息</summary><div className="plugin-technical-grid"><div><span>插件 ID</span><code>{item.id}</code></div><div><span>作者</span><strong>{item.author_name || catalogItem?.author_name || '未知作者'}</strong></div><div><span>来源</span><strong>{pluginSourceLabel(item.source)}</strong></div><div><span>运行时</span><strong>{item.runtime || '--'}</strong></div><div><span>最低桌面端版本</span><strong>{item.min_agent_version ? `v${item.min_agent_version}` : '--'}</strong></div></div></details> : null}
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

// 列表里的状态点必须反映"这个插件现在是否正常"：带未清除失败记录的插件
// 仍会出现在技能依赖里，若在列表里显示成纯绿，用户就只能在点进去之后才发现问题。
function pluginListDot(item: PluginItem) {
  if (item.status === 'failed' || item.status === 'blocked' || item.circuit_open) return 'danger';
  if (!item.enabled) return '';
  return item.error ? 'warn' : 'success';
}

// 列表行只保留需要留意的状态：默认启用是常态，不必在每一行重复说一遍。
function pluginListStateText(item: PluginItem) {
  if (item.status === 'failed' || item.status === 'blocked' || item.circuit_open) return '需要处理';
  if (item.error) return '最近调用失败';
  // 接管已安装副本时，列表行的版本号是开发版本，所以这里必须点明"装的是哪个版本"。
  if (item.development) return item.overrides_installed_version
    ? `运行本机开发版本（已安装 v${item.overrides_installed_version}）`
    : '本机开发版本';
  return item.enabled ? '' : '已停用';
}

// 健康记录会保留到下一次成功调用，所以文案必须说明"什么时候失败的"，并如实区分三种影响面：
// 陈旧失败不影响依赖者，窗口内的失败只让依赖者降级，熔断才真正阻断。
function pluginFailureHeadline(item: PluginItem) {
  const { at, stale } = pluginFailureState(item);
  const label = item.circuit_open ? '连续调用失败' : stale ? '历史调用失败' : '最近调用失败';
  const when = at ? `（${new Date(at).toLocaleString('zh-CN', { hour12: false })}）` : '';
  return `${label}${when}，共 ${item.failure_count || 1} 次：${item.error}`;
}

function pluginFailureHint(item: PluginItem) {
  const { stale } = pluginFailureState(item);
  // 熔断是冷却窗口而不是永久拉黑，所以文案要同时给出「立刻解封」和「自动恢复」两条路。
  if (item.circuit_open) return '连续失败已暂停调用，依赖它的技能暂不可用；「修复并重试」立即解封，5 分钟后也会自动重试。';
  if (stale) return '超过 24 小时，不再影响依赖它的技能；下一次成功调用会自动清除。';
  return '依赖它的技能标记为「部分功能不可用」；「修复并重试」清除记录并立即重试，成功调用后自动恢复。';
}

function pluginSourceLabel(source?: string) {
  if (source === 'github') return 'GitHub 导入';
  if (source === 'local') return '本地导入';
  if (source === 'development') return '扩展开发目录';
  if (source === 'builtin') return '系统内置';
  return source || '未标注';
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
