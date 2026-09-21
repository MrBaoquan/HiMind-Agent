import { useEffect, useMemo, useState } from 'react';
import { AppWindow, ArrowLeft, Blocks, Bot, Download, ExternalLink, FolderOpen, MonitorUp, MoreHorizontal, RefreshCw, ShieldCheck, Store, X } from 'lucide-react';
import type { CapabilityItem, CodexSkillStatusResponse, ExtensionDesiredItem, ExtensionDesiredState, PluginCatalogItem, PluginInstallPlan, PluginItem, PluginRegistry } from '../services/agentApi';
import { EmptyState, PageHeader, Pill, Tags } from '../components/Common';
import { ManagedCapabilitiesPanel } from './ManagedCapabilitiesPage';

const governanceLabels: Record<string, string> = {
  required: '系统内置',
  managed: '组织管理',
  optional: '可选插件',
  blocked: '不可安装',
};

export function PluginsPage({ loading, registry, catalog, desired, desiredLoading, desiredError, skillStatus, capabilities, dashboardEnabled, catalogEnabled, onRefresh, onLoadVersions, onPlanInstall, onInstall, onUninstall, onRollback, onSetEnabled, onOpenView, onCreateShortcut, onImportLocal, onImportGithub, onOpenExtensions }: {
  loading: boolean;
  registry: PluginRegistry | null;
  catalog: PluginCatalogItem[];
  desired: ExtensionDesiredState | null;
  desiredLoading: boolean;
  desiredError: string | null;
  skillStatus: CodexSkillStatusResponse | null;
  capabilities: CapabilityItem[];
  dashboardEnabled: boolean;
  catalogEnabled: boolean;
  onRefresh: () => void;
  onLoadVersions: (pluginId: string) => Promise<PluginCatalogItem[]>;
  onPlanInstall: (pluginId: string, version?: string) => Promise<PluginInstallPlan>;
  onInstall: (pluginId: string, version?: string) => void;
  onUninstall: (pluginId: string) => void;
  onRollback: (pluginId: string) => void;
  onSetEnabled: (pluginId: string, enabled: boolean) => void;
  onOpenView: (pluginId: string, viewId: string) => void;
  onCreateShortcut: (pluginId: string, viewId: string, title: string) => void;
  onImportLocal: () => void;
  onImportGithub: (sourceUrl: string) => Promise<void>;
  onOpenExtensions: () => void;
}) {
  const pluginItems = registry?.items || [];
  const userPluginItems = useMemo(() => dashboardEnabled
    ? pluginItems.filter(item => !isManagedPlugin(item, desired))
    : pluginItems.filter(item => item.availability !== 'control_plane'), [dashboardEnabled, desired, pluginItems]);
  const systemPluginCount = useMemo(() => {
    const ids = new Set((desired?.items || []).filter(item => item.asset_kind === 'plugin' && isManagedPolicy(item)).map(item => item.asset_key));
    pluginItems.filter(item => ['required', 'managed', 'blocked'].includes(item.governance || '')).forEach(item => ids.add(item.id));
    return ids.size;
  }, [desired, pluginItems]);
  const [selectedId, setSelectedId] = useState('');
  const [detailOpen, setDetailOpen] = useState(false);
  const [view, setView] = useState<'installed' | 'system'>(() => {
    try {
      const stored = window.localStorage.getItem('himind-agent.plugins-view');
      if (stored === 'system' && dashboardEnabled) return 'system';
      return 'installed';
    } catch { return 'installed'; }
  });
  const [installPlan, setInstallPlan] = useState<PluginInstallPlan | null>(null);
  const [planError, setPlanError] = useState('');
  const [planningId, setPlanningId] = useState('');
  const [githubOpen, setGithubOpen] = useState(false);
  const [githubSourceUrl, setGithubSourceUrl] = useState('');
  const [githubBusy, setGithubBusy] = useState(false);
  const [githubError, setGithubError] = useState('');
  const selectedPlugin = useMemo(
    () => userPluginItems.find(item => item.id === selectedId) || userPluginItems[0] || null,
    [selectedId, userPluginItems],
  );
  const selectedCapabilities = useMemo(
    () => selectedPlugin ? capabilities.filter(item => item.source === `plugin:${selectedPlugin.id}` || selectedPlugin.capabilities?.some(capability => capability.id === item.id)) : [],
    [capabilities, selectedPlugin],
  );
  const installedById = new Map(pluginItems.map(item => [item.id, item]));

  useEffect(() => { setDetailOpen(false); }, [view]);
  useEffect(() => { try { window.localStorage.setItem('himind-agent.plugins-view', view); } catch { /* storage is optional */ } }, [view]);
  useEffect(() => {
    if (!dashboardEnabled && view === 'system') setView('installed');
  }, [dashboardEnabled, view]);

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
    <PageHeader title="插件" description="管理已安装插件" actions={<div className="actions-row"><button className="btn" onClick={onOpenExtensions}><Store size={15} />浏览市场</button><details className="runtime-more-actions"><summary title="更多操作" aria-label="更多操作"><MoreHorizontal size={17} /></summary><div><button type="button" onClick={() => { setGithubError(''); setGithubOpen(true); }}><ExternalLink size={14} />从 GitHub 导入</button><button type="button" onClick={onImportLocal}><FolderOpen size={14} />从本地导入</button></div></details><button className="btn btn-icon" title="刷新插件状态" aria-label="刷新插件状态" onClick={onRefresh}><RefreshCw size={16} /></button></div>} />
      <div className="plugin-toolbar"><div className="plugin-tabs" role="tablist" aria-label="插件视图"><button role="tab" aria-selected={view === 'installed'} className={view === 'installed' ? 'active' : ''} onClick={() => setView('installed')}>已安装 <span>{userPluginItems.length}</span></button>{dashboardEnabled ? <button role="tab" aria-selected={view === 'system'} className={view === 'system' ? 'active' : ''} onClick={() => setView('system')}>组织管理 <span>{systemPluginCount}</span></button> : null}</div><div className="plugin-toolbar-meta"><span className={`status-dot ${registry?.registry_ready ? 'success' : 'danger'}`} /><span>{registry?.registry_ready ? '插件可用' : '插件需要处理'}</span></div></div>
      {dashboardEnabled && view === 'system' ? <ManagedCapabilitiesPanel assetKind="plugin" desired={desired} loading={desiredLoading} error={desiredError} registry={registry} skillStatus={skillStatus} /> : <>
      <div className={`plugin-workspace compact-master-detail ${detailOpen ? 'detail-open' : ''}`}>
<aside className="plugin-list" aria-label="已安装插件列表">
          <div className="plugin-list-header"><strong>已安装</strong><span className="section-count">{userPluginItems.length}</span></div>
          <div className="plugin-list-body">
            {userPluginItems.map(item => (
              <button key={item.id} type="button" className={`plugin-list-item ${selectedPlugin?.id === item.id ? 'selected' : ''}`} onClick={() => { setSelectedId(item.id); setDetailOpen(true); }}>
                <span className={`status-dot ${item.status === 'failed' ? 'danger' : item.enabled ? 'success' : ''}`} />
                <span><strong>{item.name || item.id}</strong><small>作者：{item.author_name || catalog.find(candidate => candidate.plugin_id === item.id)?.author_name || '未知作者'}</small><small>{item.circuit_open || item.status === 'failed' ? '需要处理' : item.enabled ? '已启用' : '已停用'}</small></span>
                <small>v{item.version || '--'}</small>
              </button>
            ))}
{userPluginItems.length === 0 ? <EmptyState icon={Blocks} title="还没有安装插件" text="从市场安装，或从更多操作导入已有插件。" /> : null}
          </div>
        </aside>
      <main className="plugin-detail">
          <button className="workspace-back" onClick={() => setDetailOpen(false)}><ArrowLeft size={15} />返回插件列表</button>
           {selectedPlugin ? <PluginDetail key={selectedPlugin.id} item={selectedPlugin} capabilities={selectedCapabilities} catalog={catalog} catalogEnabled={catalogEnabled} onLoadVersions={onLoadVersions} onPlanVersion={(version) => void openInstallPlan(selectedPlugin.id, version)} onUninstall={onUninstall} onRollback={onRollback} onSetEnabled={onSetEnabled} onOpenView={onOpenView} onCreateShortcut={onCreateShortcut} /> : <EmptyState icon={Blocks} title="选择插件" text="查看功能、依赖和版本。" />}
        </main>
      </div></>}
      {installPlan || planError ? <PluginInstallPlanDialog plan={installPlan} error={planError} currentVersion={installPlan ? installedById.get(installPlan.plugin.plugin_id)?.version : undefined} onClose={() => { setInstallPlan(null); setPlanError(''); }} onInstall={() => { if (installPlan) onInstall(installPlan.plugin.plugin_id, installPlan.plugin.version); setInstallPlan(null); }} /> : null}
      {githubOpen ? <div className="modal-backdrop" role="presentation"><div className="modal" role="dialog" aria-modal="true" aria-labelledby="github-plugin-title"><div className="modal-header"><div><h3 id="github-plugin-title">从 GitHub 导入插件</h3><p>粘贴仓库链接即可；子目录和版本可按 UPM 方式写在链接中。</p></div><button className="btn btn-icon" aria-label="关闭" title="关闭" onClick={() => setGithubOpen(false)}><X size={16} /></button></div><div className="modal-body"><div className="field-group"><label className="field-label" htmlFor="github-plugin-source-url">GitHub 链接</label><input id="github-plugin-source-url" value={githubSourceUrl} onChange={event => setGithubSourceUrl(event.target.value)} placeholder="https://github.com/owner/repository.git?path=/plugins/example#v1.0.0" /></div>{githubError ? <div className="inline-feedback visible" role="status">{githubError}</div> : null}<div className="modal-actions"><span /><div className="actions-row"><button className="btn" onClick={() => setGithubOpen(false)}>取消</button><button className="btn btn-primary" disabled={githubBusy || !githubSourceUrl.trim()} onClick={async () => { setGithubBusy(true); setGithubError(''); try { await onImportGithub(githubSourceUrl.trim()); setGithubOpen(false); } catch (error) { setGithubError(error instanceof Error ? error.message : 'GitHub 插件导入失败'); } finally { setGithubBusy(false); } }}>{githubBusy ? '导入中...' : '导入插件'}</button></div></div></div></div></div> : null}
    </div>
  );
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

function PluginDetail({ item, capabilities, catalog, catalogEnabled, onLoadVersions, onPlanVersion, onUninstall, onRollback, onSetEnabled, onOpenView, onCreateShortcut }: {
  item: PluginItem;
  capabilities: CapabilityItem[];
  catalog: PluginCatalogItem[];
  catalogEnabled: boolean;
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

  return (
    <>
      <div className="plugin-detail-header">
        <div><div className="plugin-title-line"><h3>{item.name || item.id}</h3><Pill kind={item.circuit_open ? 'danger' : item.status === 'installed' && item.enabled ? 'success' : item.status === 'failed' ? 'danger' : 'warn'}>{item.circuit_open || item.status === 'failed' ? '运行异常' : item.enabled ? '已安装' : '已停用'}</Pill>{item.development ? <Pill kind="warn">开发中</Pill> : null}{item.governance ? <Pill kind={item.governance === 'required' || item.governance === 'managed' ? 'warn' : 'success'}>{governanceLabels[item.governance] || item.governance}</Pill> : null}</div></div>
        <div className="plugin-installed-actions">{!item.development ? <><label className="plugin-enable-control"><span>启用</span><span className="toggle"><input type="checkbox" checked={Boolean(item.enabled)} disabled={item.governance === 'required' || item.governance === 'managed'} onChange={event => event.target.checked ? onSetEnabled(item.id, true) : setPendingAction({ title: '确认停用插件？', description: `停用后，Agent 将不再调用“${item.name || item.id}”提供的能力。`, confirmText: '确认停用', run: () => onSetEnabled(item.id, false) })} /><span className="slider" /></span></label><details className="plugin-more-actions"><summary title="更多操作" aria-label="更多操作"><MoreHorizontal size={17} /></summary><div>{item.governance !== 'required' && item.governance !== 'managed' && item.governance !== 'blocked' ? <button disabled={!item.rollback_available} onClick={() => setPendingAction({ title: '确认回滚插件？', description: `将“${item.name || item.id}”切换到 v${item.previous_version || '--'}，当前版本会停止运行。`, confirmText: '确认回滚', run: () => onRollback(item.id) })}>回滚{item.previous_version ? `到 v${item.previous_version}` : ''}</button> : null}<button className="danger-text" disabled={item.governance === 'required' || item.governance === 'managed'} onClick={() => setPendingAction({ title: '确认卸载插件？', description: `卸载后将移除“${item.name || item.id}”，其能力和功能页面会立即不可用。`, confirmText: '确认卸载', run: () => onUninstall(item.id) })}>卸载</button></div></details></> : null}</div>
      </div>
      {item.error ? <div className="plugin-local-error">插件暂时无法运行，请刷新状态或重新安装。</div> : null}
      {item.development ? <div className="plugin-product-notice"><ShieldCheck size={16} /><div><strong>本机开发版本</strong><span>项目构建、测试和移除统一在“扩展开发”中管理。</span></div></div> : null}
      <p className="plugin-product-description">{item.description || '暂无说明。'}</p>
       {!item.development && catalogEnabled ? <DetailTabs value={tab} onChange={setTab} /> : null}
       {!item.development && catalogEnabled && tab === 'versions' ? <PluginVersionList versions={versions.length ? versions : catalogItem ? [catalogItem] : []} currentVersion={item.version} lockedLabel={item.governance === 'blocked' ? '不可安装' : managed ? '组织管理' : undefined} loading={versionsLoading} error={versionsError} onSelect={onPlanVersion} /> : <>
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

function compareSemanticVersions(left: string, right: string) {
  const parse = (value: string) => value.split(/[.+-]/).slice(0, 3).map(part => Number.parseInt(part, 10) || 0);
  const leftParts = parse(left);
  const rightParts = parse(right);
  for (let index = 0; index < 3; index += 1) {
    if ((leftParts[index] || 0) !== (rightParts[index] || 0)) return (leftParts[index] || 0) - (rightParts[index] || 0);
  }
  return 0;
}
