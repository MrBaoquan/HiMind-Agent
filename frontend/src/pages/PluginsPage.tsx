import { useMemo, useState } from 'react';
import { Blocks, ExternalLink, FolderOpen, MonitorUp, RefreshCw } from 'lucide-react';
import type { CapabilityItem, PluginItem, PluginRegistry } from '../services/agentApi';
import { EmptyState, IconButton, PageHeader, Pill, Tags } from '../components/Common';

export function PluginsPage({ registry, capabilities, onRefresh, onOpenDirectory, onOpenView, onCreateShortcut }: {
  registry: PluginRegistry | null;
  capabilities: CapabilityItem[];
  onRefresh: () => void;
  onOpenDirectory: () => void;
  onOpenView: (pluginId: string, viewId: string) => void;
  onCreateShortcut: (pluginId: string, viewId: string, title: string) => void;
}) {
  const pluginItems = registry?.items || [];
  const [selectedId, setSelectedId] = useState('');
  const selectedPlugin = useMemo(
    () => pluginItems.find(item => item.id === selectedId) || pluginItems[0] || null,
    [pluginItems, selectedId],
  );
  const selectedCapabilities = useMemo(
    () => selectedPlugin ? capabilities.filter(item => item.source === `plugin:${selectedPlugin.id}`) : [],
    [capabilities, selectedPlugin],
  );
  const pluginCapabilityCount = capabilities.filter(item => String(item.source || '').startsWith('plugin:')).length;

  return (
    <div className="plugin-page">
      <PageHeader
        title="本机插件"
        description="打开当前设备已安装的插件，并查看其本机权限与能力。安装和版本策略由 Dashboard 管理。"
        actions={<><IconButton icon={RefreshCw} label="刷新插件" onClick={onRefresh} /><button className="btn" onClick={onOpenDirectory} disabled={!registry?.registry_dir}><FolderOpen size={16} />插件目录</button></>}
      />
      <div className="plugin-runtime-strip">
        <div><span className={`status-dot ${registry?.registry_ready ? 'success' : 'danger'}`} /><strong>{registry?.registry_ready ? '注册表就绪' : '注册表未就绪'}</strong></div>
        <div><strong>{registry?.total ?? pluginItems.length}</strong><span>本机插件</span></div>
        <div><strong>{pluginCapabilityCount}</strong><span>可调用能力</span></div>
        <code title={registry?.registry_dir}>{registry?.registry_dir || '--'}</code>
      </div>
      <div className="plugin-workspace">
        <aside className="plugin-list" aria-label="本机插件列表">
          <div className="plugin-list-header"><strong>已安装</strong><span className="section-count">{pluginItems.length}</span></div>
          <div className="plugin-list-body">
            {pluginItems.map(item => (
              <button key={item.id} type="button" className={`plugin-list-item ${selectedPlugin?.id === item.id ? 'selected' : ''}`} onClick={() => setSelectedId(item.id)}>
                <span className={`status-dot ${item.status === 'failed' ? 'danger' : item.enabled ? 'success' : ''}`} />
                <span><strong>{item.name || item.id}</strong><small>{item.id}</small></span>
                <small>v{item.version || '--'}</small>
              </button>
            ))}
            {pluginItems.length === 0 ? <EmptyState icon={Blocks} title="暂无本机插件" text="插件由 Dashboard 分发或安装到本机注册表目录。" /> : null}
          </div>
        </aside>
        <main className="plugin-detail">
          {selectedPlugin ? <PluginDetail item={selectedPlugin} capabilities={selectedCapabilities} onOpenView={onOpenView} onCreateShortcut={onCreateShortcut} /> : <EmptyState icon={Blocks} title="选择插件" text="选择左侧插件后查看本机运行信息。" />}
        </main>
      </div>
    </div>
  );
}

function PluginDetail({ item, capabilities, onOpenView, onCreateShortcut }: {
  item: PluginItem;
  capabilities: CapabilityItem[];
  onOpenView: (pluginId: string, viewId: string) => void;
  onCreateShortcut: (pluginId: string, viewId: string, title: string) => void;
}) {
  return (
    <>
      <div className="plugin-detail-header">
        <div><div className="plugin-title-line"><h3>{item.name || item.id}</h3><Pill kind={item.status === 'installed' && item.enabled ? 'success' : item.status === 'failed' ? 'danger' : 'warn'}>{item.enabled ? item.status : 'disabled'}</Pill></div><code>{item.id}</code></div>
      </div>
      {item.error ? <div className="plugin-local-error">{item.error}</div> : null}
      <div className="plugin-meta-grid">
        <div><span>版本</span><strong>{item.version || '--'}</strong></div>
        <div><span>运行时</span><strong>{item.runtime || '--'}</strong></div>
        <div><span>功能页面</span><strong>{item.views?.length || 0}</strong></div>
        <div><span>能力</span><strong>{capabilities.length}</strong></div>
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
        {capabilities.length ? <div className="plugin-capability-list">{capabilities.map(item => <div className="plugin-capability-row" key={item.id}><code>{item.id}</code><span>{item.risk_level || '--'}</span><p>{item.description || '--'}</p></div>)}</div> : <div className="plugin-section-empty">此插件没有已注册能力</div>}
      </section>
    </>
  );
}
