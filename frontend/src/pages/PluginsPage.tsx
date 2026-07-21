import { useMemo, useState } from 'react';
import { Blocks, Check, Download, ExternalLink, FolderOpen, FlaskConical, LockKeyhole, MonitorUp, Play, RefreshCw, Search, ShieldCheck, Unplug, X } from 'lucide-react';
import type { CapabilityItem, DevelopmentInvocationResult, PluginCapability, PluginCatalogItem, PluginItem, PluginJsonSchema, PluginRegistry } from '../services/agentApi';
import { EmptyState, Pill, Tags } from '../components/Common';

const governanceLabels: Record<string, string> = {
  required: '核心插件',
  managed: '组织管理',
  optional: '可选插件',
  blocked: '不可安装',
};

export function PluginsPage({ loading, registry, catalog, capabilities, onRefresh, onInstall, onUninstall, onRollback, onSetEnabled, onOpenDirectory, onRegisterDevelopment, onUnregisterDevelopment, onInvokeDevelopment, onOpenView, onCreateShortcut }: {
  loading: boolean;
  registry: PluginRegistry | null;
  catalog: PluginCatalogItem[];
  capabilities: CapabilityItem[];
  onRefresh: () => void;
  onInstall: (pluginId: string) => void;
  onUninstall: (pluginId: string) => void;
  onRollback: (pluginId: string) => void;
  onSetEnabled: (pluginId: string, enabled: boolean) => void;
  onOpenDirectory: () => void;
  onRegisterDevelopment: () => void;
  onUnregisterDevelopment: (pluginId: string) => void;
  onInvokeDevelopment: (pluginId: string, capabilityId: string, input: unknown) => Promise<DevelopmentInvocationResult>;
  onOpenView: (pluginId: string, viewId: string) => void;
  onCreateShortcut: (pluginId: string, viewId: string, title: string) => void;
}) {
  const pluginItems = registry?.items || [];
  const [selectedId, setSelectedId] = useState('');
  const [view, setView] = useState<'market' | 'installed'>('market');
  const [query, setQuery] = useState('');
  const selectedPlugin = useMemo(
    () => pluginItems.find(item => item.id === selectedId) || pluginItems[0] || null,
    [pluginItems, selectedId],
  );
  const selectedCapabilities = useMemo(
    () => selectedPlugin ? capabilities.filter(item => item.source === `plugin:${selectedPlugin.id}` || selectedPlugin.capabilities?.some(capability => capability.id === item.id)) : [],
    [capabilities, selectedPlugin],
  );
  const pluginCapabilityCount = capabilities.filter(item => String(item.source || '').startsWith('plugin:')).length;
  const installedById = new Map(pluginItems.map(item => [item.id, item]));
  const visibleCatalog = catalog.filter(item => `${item.name} ${item.plugin_id} ${item.description}`.toLowerCase().includes(query.trim().toLowerCase()));
  const requiredCount = catalog.filter(item => item.governance === 'required').length;
  const optionalCount = catalog.filter(item => item.governance === 'optional').length;

  if (loading && !registry && catalog.length === 0) return <div className="page-loading"><span className="spinner" />正在读取插件市场与本机注册表</div>;

  return (
    <div className="plugin-page">
      <header className="plugin-hero">
        <div className="plugin-hero-copy"><span className="plugin-eyebrow"><Blocks size={14} /> HiMind Extensions</span><h2>扩展你的本机 Agent</h2><p>从受信任的插件市场选择工具。安装包会在本机完成签名、摘要和兼容性校验。</p></div>
        <div className="plugin-hero-actions"><button className="btn btn-primary" onClick={onRegisterDevelopment}><FlaskConical size={15} />加载本地工程</button><button className="btn btn-icon" title="重新加载插件" aria-label="重新加载插件" onClick={onRefresh}><RefreshCw size={16} /></button><button className="btn" onClick={onOpenDirectory} disabled={!registry?.registry_dir}><FolderOpen size={15} />本机目录</button></div>
      </header>
      <div className="plugin-summary-strip"><div><span className="plugin-summary-icon blue"><Blocks size={16} /></span><span><small>市场插件</small><strong>{catalog.length}</strong></span></div><div><span className="plugin-summary-icon green"><Check size={16} /></span><span><small>已安装</small><strong>{registry?.total ?? pluginItems.length}</strong></span></div><div><span className="plugin-summary-icon amber"><ShieldCheck size={16} /></span><span><small>核心依赖</small><strong>{requiredCount}</strong></span></div><div><span className={`status-dot ${registry?.registry_ready ? 'success' : 'danger'}`} /><span><small>本机注册表</small><strong>{registry?.registry_ready ? '正常' : '异常'}</strong></span></div></div>
      <div className="plugin-toolbar"><div className="plugin-tabs" role="tablist" aria-label="插件视图"><button role="tab" aria-selected={view === 'market'} className={view === 'market' ? 'active' : ''} onClick={() => setView('market')}>发现插件 <span>{catalog.length}</span></button><button role="tab" aria-selected={view === 'installed'} className={view === 'installed' ? 'active' : ''} onClick={() => setView('installed')}>已安装 <span>{pluginItems.length}</span></button></div>{view === 'market' ? <label className="plugin-search"><Search size={15} /><span className="sr-only">搜索插件</span><input value={query} onChange={event => setQuery(event.target.value)} placeholder="搜索插件名称或能力" /></label> : <div className="plugin-toolbar-meta"><span>{pluginCapabilityCount} 项可调用能力</span><code title={registry?.registry_dir}>{registry?.registry_dir || '--'}</code></div>}</div>
      {view === 'market' ? <section className="plugin-market-area"><div className="plugin-section-title"><div><h3>插件市场</h3><p>{optionalCount} 个可选插件 · {requiredCount} 个核心依赖</p></div><span>{visibleCatalog.length} 个结果</span></div><div className="plugin-market-grid">{visibleCatalog.map(item => { const installed = installedById.get(item.plugin_id); const upgrade = installed?.version ? compareSemanticVersions(item.version, installed.version) > 0 : false; return <article className="plugin-market-card" key={item.plugin_id}><div className="plugin-card-top"><span className="plugin-card-mark">{item.name.slice(0, 1)}</span><div><strong>{item.name}</strong><small>{item.plugin_id}</small></div><Pill kind={item.governance === 'required' ? 'warn' : item.governance === 'blocked' ? 'danger' : 'success'}>{governanceLabels[item.governance] || item.governance}</Pill></div><p className="plugin-card-description">{item.description || '暂无插件说明。'}</p><div className="plugin-card-meta"><span>版本 <strong>v{item.version}</strong></span><span>作者 <strong>{item.author_name || '平台团队'}</strong></span></div><details className="plugin-card-details"><summary>兼容性与制品校验</summary><div><span>最低 Agent</span><strong>v{item.min_agent_version || '--'}</strong></div><div><span>制品大小</span><strong>{formatFileSize(item.file_size)}</strong></div><div><span>SHA-256</span><code title={item.sha256}>{item.sha256?.slice(0, 16) || '--'}{item.sha256 ? '...' : ''}</code></div><p>{item.release_notes || '未提供版本更新说明。'}</p></details><div className="plugin-card-footer">{item.governance === 'required' ? <span><LockKeyhole size={13} />系统依赖</span> : item.governance === 'blocked' ? <span className="danger-text"><LockKeyhole size={13} />策略禁止</span> : upgrade ? <span><MonitorUp size={13} />可升级</span> : <span><Download size={13} />可安装</span>}<button className="btn btn-primary" disabled={(Boolean(installed) && !upgrade) || item.governance === 'blocked'} onClick={() => onInstall(item.plugin_id)}>{item.governance === 'blocked' ? '不可安装' : upgrade ? '升级' : installed ? '已是最新' : '安装'}</button></div></article>; })}</div>{visibleCatalog.length === 0 ? <EmptyState icon={Search} title="没有匹配的插件" text={catalog.length === 0 ? 'Dashboard 尚未上架可供当前 Agent 使用的插件。' : '尝试更换搜索关键词。'} /> : null}</section> : <>
      <div className="plugin-workspace">
        <aside className="plugin-list" aria-label="本机插件列表">
          <div className="plugin-list-header"><strong>已安装</strong><span className="section-count">{pluginItems.length}</span></div>
          <div className="plugin-list-body">
            {pluginItems.map(item => (
              <button key={item.id} type="button" className={`plugin-list-item ${selectedPlugin?.id === item.id ? 'selected' : ''}`} onClick={() => setSelectedId(item.id)}>
                <span className={`status-dot ${item.status === 'failed' ? 'danger' : item.enabled ? 'success' : ''}`} />
                <span><strong>{item.name || item.id}</strong><small>{item.circuit_open ? '已熔断' : item.id}</small></span>
                <small>v{item.version || '--'}</small>
              </button>
            ))}
            {pluginItems.length === 0 ? <EmptyState icon={Blocks} title="暂无本机插件" text="插件由 Dashboard 分发或安装到本机注册表目录。" /> : null}
          </div>
        </aside>
        <main className="plugin-detail">
          {selectedPlugin ? <PluginDetail key={selectedPlugin.id} item={selectedPlugin} capabilities={selectedCapabilities} onUninstall={onUninstall} onRollback={onRollback} onSetEnabled={onSetEnabled} onUnregisterDevelopment={onUnregisterDevelopment} onInvokeDevelopment={onInvokeDevelopment} onOpenView={onOpenView} onCreateShortcut={onCreateShortcut} /> : <EmptyState icon={Blocks} title="选择插件" text="选择左侧插件后查看本机运行信息。" />}
        </main>
      </div></>}
    </div>
  );
}

function PluginDetail({ item, capabilities, onUninstall, onRollback, onSetEnabled, onUnregisterDevelopment, onInvokeDevelopment, onOpenView, onCreateShortcut }: {
  item: PluginItem;
  capabilities: CapabilityItem[];
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

  return (
    <>
      <div className="plugin-detail-header">
        <div><div className="plugin-title-line"><h3>{item.name || item.id}</h3><Pill kind={item.circuit_open ? 'danger' : item.status === 'installed' && item.enabled ? 'success' : item.status === 'failed' ? 'danger' : 'warn'}>{item.circuit_open ? '已熔断' : item.enabled ? item.status === 'installed' ? '运行中' : item.status : '已停用'}</Pill>{item.failure_count ? <Pill kind="danger">失败 {item.failure_count} 次</Pill> : null}{item.development ? <Pill kind="warn">开发中</Pill> : null}{item.governance ? <Pill kind={item.governance === 'required' || item.governance === 'managed' ? 'warn' : 'success'}>{governanceLabels[item.governance] || item.governance}</Pill> : null}</div><code>{item.id}</code>{item.development ? <code title={item.path}>{item.path}</code> : null}</div>
        <div className="actions-row">{item.development ? <button className="btn btn-danger-quiet" onClick={() => setPendingAction({ title: '移除开发插件注册？', description: `移除后将停止加载“${item.name || item.id}”的本地工程，但不会删除工程文件。`, confirmText: '确认移除', run: () => onUnregisterDevelopment(item.id) })}><Unplug size={15} />移除开发注册</button> : <><button className="btn" disabled={item.governance === 'required' || item.governance === 'managed'} title="核心或组织管理插件不可停用" onClick={() => item.enabled ? setPendingAction({ title: '确认停用插件？', description: `停用后，Agent 将不再调用“${item.name || item.id}”提供的能力。`, confirmText: '确认停用', run: () => onSetEnabled(item.id, false) }) : onSetEnabled(item.id, true)}>{item.enabled ? '停用' : '启用'}</button>{item.governance !== 'required' && item.governance !== 'managed' && item.governance !== 'blocked' ? <button className="btn" disabled={!item.rollback_available} title={item.rollback_available ? `切换到 ${item.previous_version}` : '暂无可用上一版本'} onClick={() => setPendingAction({ title: '确认回滚插件？', description: `将“${item.name || item.id}”切换到 v${item.previous_version || '--'}，当前版本会停止运行。`, confirmText: '确认回滚', run: () => onRollback(item.id) })}>回滚{item.previous_version ? `到 v${item.previous_version}` : ''}</button> : null}<button className="btn btn-danger-quiet" disabled={item.governance === 'required' || item.governance === 'managed'} title="核心或组织管理插件不可卸载" onClick={() => setPendingAction({ title: '确认卸载插件？', description: `卸载后将移除“${item.name || item.id}”，其能力和功能页面会立即不可用。`, confirmText: '确认卸载', run: () => onUninstall(item.id) })}>卸载</button></>}</div>
      </div>
      {item.error ? <div className="plugin-local-error">{item.error}</div> : null}
      <div className="plugin-meta-grid">
        <div><span>版本</span><strong>{item.version || '--'}</strong></div>
        <div><span>运行时</span><strong>{item.runtime || '--'}</strong></div>
        <div><span>{item.development ? '入口构建时间' : '功能页面'}</span><strong>{item.development ? formatBuildTime(item.entry_modified_at) : item.views?.length || 0}</strong></div>
        <div><span>{item.development ? '入口大小' : '能力'}</span><strong>{item.development ? formatFileSize(item.entry_size) : capabilities.length}</strong></div>
        {!item.development ? <div><span>上一版本</span><strong>{item.previous_version ? `v${item.previous_version}` : '--'}</strong></div> : null}
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
        <div className="plugin-section-heading"><div><h4>本机权限</h4><p>插件 Manifest 声明的设备访问范围。</p></div><span className="section-count">{item.permissions?.length || 0}</span></div>
        <div className="plugin-permission-list"><Tags items={item.permissions || []} /></div>
      </section>
      <section className="plugin-detail-section plugin-capability-section">
        <div className="plugin-section-heading"><div><h4>插件能力</h4><p>当前插件提供给 Agent 与 MCP 的能力。</p></div><span className="section-count">{capabilities.length}</span></div>
        {capabilities.length ? <div className="plugin-capability-list">{capabilities.map(item => <div className="plugin-capability-row" key={item.id}><code>{item.id}</code><span>{riskLevelLabel(item.risk_level)}</span><p>{item.description || '--'}</p></div>)}</div> : <div className="plugin-section-empty">此插件没有已注册能力</div>}
      </section>
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
