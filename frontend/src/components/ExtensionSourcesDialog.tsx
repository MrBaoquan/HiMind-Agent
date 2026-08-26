import { useEffect, useMemo, useState } from 'react';
import { CircleAlert, FolderOpen, GitBranch, MessageCircle, Plus, RefreshCw, Trash2, X } from 'lucide-react';
import type { ExtensionSourceConfig, ExtensionSourceSettings, ExtensionSourceSnapshot, ExtensionWorkspaceSettings } from '../services/agentApi';

type Props = {
  open: boolean;
  workspace: ExtensionWorkspaceSettings;
  settings: ExtensionSourceSettings;
  snapshot: ExtensionSourceSnapshot | null;
  loading: boolean;
  error: string;
  onClose: () => void;
  onSelectWorkspace: () => Promise<void>;
  onDevelopWorkspace: () => void;
  onRefresh: () => Promise<void>;
  onAdd: (name: string, repository: string, reference: string, catalogPath: string, verification: ExtensionSourceConfig['verification']) => Promise<void>;
  onUpdate: (source: ExtensionSourceConfig, enabled: boolean, autoUpdate: boolean, verification: ExtensionSourceConfig['verification']) => Promise<void>;
  onRemove: (sourceId: string) => Promise<void>;
};

export function ExtensionSourcesDialog({ open, workspace, settings, snapshot, loading, error, onClose, onSelectWorkspace, onDevelopWorkspace, onRefresh, onAdd, onUpdate, onRemove }: Props) {
  const [repository, setRepository] = useState('');
  const [name, setName] = useState('');
  const [reference, setReference] = useState('main');
  const [catalogPath, setCatalogPath] = useState('.himind/catalog.json');
  const [verification, setVerification] = useState<ExtensionSourceConfig['verification']>('required');
  const [formOpen, setFormOpen] = useState(false);
  const [localError, setLocalError] = useState('');
  const statuses = useMemo(() => new Map((snapshot?.sources || []).map(item => [item.source.id, item])), [snapshot]);

  useEffect(() => {
    if (!open) return;
    setLocalError('');
    void onRefresh().catch(reason => setLocalError(messageOf(reason)));
    // The parent callback is intentionally read only when the dialog opens.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  if (!open) return null;

  async function addSource() {
    setLocalError('');
    try {
      await onAdd(name.trim(), repository.trim(), reference.trim(), catalogPath.trim(), verification);
      setName('');
      setRepository('');
      setReference('main');
      setCatalogPath('.himind/catalog.json');
      setVerification('required');
      setFormOpen(false);
    } catch (reason) {
      setLocalError(messageOf(reason));
    }
  }

  async function addOfficialSource() {
    setLocalError('');
    try {
      await onAdd('HiMind 扩展', 'MrBaoquan/himind-extensions', 'main', '.himind/catalog.json', 'required');
    } catch (reason) {
      setLocalError(messageOf(reason));
    }
  }

  async function updateSource(source: ExtensionSourceConfig, enabled: boolean, autoUpdate: boolean, verification: ExtensionSourceConfig['verification']) {
    setLocalError('');
    try { await onUpdate(source, enabled, autoUpdate, verification); }
    catch (reason) { setLocalError(messageOf(reason)); }
  }

  async function removeSource(sourceId: string) {
    setLocalError('');
    try { await onRemove(sourceId); }
    catch (reason) { setLocalError(messageOf(reason)); }
  }

  return <div className="modal-backdrop extension-source-backdrop" role="presentation">
    <div className="modal extension-source-dialog" role="dialog" aria-modal="true" aria-labelledby="extension-source-title">
      <div className="modal-header">
        <div><h3 id="extension-source-title">扩展源</h3><p>开发仓库与 GitHub 分发</p></div>
        <button className="btn btn-icon" title="关闭" aria-label="关闭" onClick={onClose}><X size={16} /></button>
      </div>
      <div className="modal-body extension-source-body">
        <section className="extension-source-workspace" aria-label="本地开发仓库">
          <div className="extension-source-workspace-main">
            <span className="extension-source-mark"><GitBranch size={17} /></span>
            <div><strong>本地开发仓库</strong>{workspace.valid ? <><code title={workspace.root}>{workspace.root}</code><small>{workspace.repository || workspaceName(workspace.root)} · {workspace.default_branch} · {workspace.extension_count} 个扩展</small></> : <small className={workspace.configured ? 'is-warning' : ''}>{workspace.error || '选择包含 extensions.json 的聚合仓库'}</small>}</div>
          </div>
          <div className="extension-source-workspace-actions">
            {workspace.valid ? <button className="btn btn-icon btn-primary" title="使用 HiMind AI 开发仓库" aria-label="使用 HiMind AI 开发仓库" disabled={loading} onClick={onDevelopWorkspace}><MessageCircle size={15} /></button> : null}
            <button className={`btn btn-icon ${workspace.valid ? '' : 'btn-primary'}`} title={workspace.valid ? '更换本地开发仓库' : '选择本地开发仓库'} aria-label={workspace.valid ? '更换本地开发仓库' : '选择本地开发仓库'} disabled={loading} onClick={() => void onSelectWorkspace()}><FolderOpen size={15} /></button>
          </div>
        </section>
        <div className="extension-source-toolbar">
          <span>GitHub 分发源 · {settings.sources.length}</span>
          <div className="actions-row">
            <button className="btn btn-icon" title="刷新扩展源" aria-label="刷新扩展源" disabled={loading} onClick={() => void onRefresh().catch(reason => setLocalError(messageOf(reason)))}><RefreshCw className={loading ? 'spin' : ''} size={16} /></button>
            <button className="btn btn-primary" disabled={loading} onClick={() => setFormOpen(current => !current)}><Plus size={15} />添加</button>
          </div>
        </div>

        {formOpen ? <section className="extension-source-form">
          <div className="field-group"><label className="field-label" htmlFor="extension-source-repository">GitHub 仓库链接</label><input id="extension-source-repository" value={repository} onChange={event => setRepository(event.target.value)} placeholder="https://github.com/owner/repository" /></div>
          <details className="extension-source-advanced"><summary>高级设置</summary><div className="extension-source-advanced-fields">
            <div className="extension-source-form-row">
              <div className="field-group"><label className="field-label" htmlFor="extension-source-name">名称</label><input id="extension-source-name" value={name} onChange={event => setName(event.target.value)} placeholder="自动使用仓库名称" /></div>
              <div className="field-group"><label className="field-label" htmlFor="extension-source-reference">分支或 Tag</label><input id="extension-source-reference" value={reference} onChange={event => setReference(event.target.value)} placeholder="main" /></div>
            </div>
            <div className="field-group"><label className="field-label" htmlFor="extension-source-catalog">目录文件</label><input id="extension-source-catalog" value={catalogPath} onChange={event => setCatalogPath(event.target.value)} placeholder=".himind/catalog.json" /></div>
            <div className="field-group"><label className="field-label" htmlFor="extension-source-verification">来源校验</label><select id="extension-source-verification" value={verification} onChange={event => setVerification(event.target.value as ExtensionSourceConfig['verification'])}><option value="required">仅安装可信签名</option><option value="optional">允许用户自定义制品</option></select><small>选择用户自定义时，已有签名仍会严格校验。</small></div>
          </div></details>
          <div className="extension-source-form-actions"><button className="btn" onClick={() => setFormOpen(false)}>取消</button><button className="btn btn-primary" disabled={loading || !repository.trim() || !reference.trim() || !catalogPath.trim()} onClick={() => void addSource()}>保存</button></div>
        </section> : null}

        {(localError || error) ? <div className="skill-inline-warning"><CircleAlert size={15} /><span>{localError || error}</span></div> : null}

        <div className="extension-source-list">
          {settings.sources.map(source => {
            const status = statuses.get(source.id);
            const ready = status?.state === 'ready';
            const official = source.repository.toLowerCase() === 'mrbaoquan/himind-extensions';
            return <article className="extension-source-item" key={source.id}>
              <div className="extension-source-item-main">
                <span className="extension-source-mark"><GitBranch size={17} /></span>
                <div><strong>{source.name || source.repository}</strong><code>{source.repository}</code><small>{source.reference} · {source.catalog_path}</small></div>
              </div>
              <div className="extension-source-item-status">
                <span><span className={`status-dot ${ready ? 'success' : source.enabled ? 'danger' : ''}`} />{!source.enabled ? '已停用' : ready ? (status?.using_cache ? '缓存可用' : '可用') : status ? '不可用' : '待刷新'}</span>
                {status ? <small>{status.plugin_count} 个插件 · {status.skill_count} 个技能</small> : null}
                <small>{source.verification === 'optional' ? '用户自定义来源' : '可信签名'}</small>
              </div>
              <div className="extension-source-controls">
                <label><span>启用</span><span className="toggle"><input type="checkbox" checked={source.enabled} disabled={loading} onChange={event => void updateSource(source, event.target.checked, source.auto_update, source.verification)} /><span className="slider" /></span></label>
                <label><span>自动更新</span><span className="toggle"><input type="checkbox" checked={source.auto_update} disabled={loading || !source.enabled} onChange={event => void updateSource(source, source.enabled, event.target.checked, source.verification)} /><span className="slider" /></span></label>
                <select className="extension-source-verification-select" title={official ? 'HiMind 官方源固定使用可信签名' : '来源校验'} aria-label={`${source.name || source.repository} 来源校验`} value={source.verification} disabled={loading || official} onChange={event => void updateSource(source, source.enabled, source.auto_update, event.target.value as ExtensionSourceConfig['verification'])}><option value="required">可信签名</option><option value="optional">用户自定义</option></select>
                <button className="btn btn-icon btn-danger-quiet" title="移除扩展源" aria-label={`移除 ${source.name || source.repository}`} disabled={loading} onClick={() => void removeSource(source.id)}><Trash2 size={15} /></button>
              </div>
              {source.enabled && status?.error ? <div className="extension-source-item-error">{status.error}</div> : null}
            </article>;
          })}
          {!settings.sources.length ? <div className="extension-source-empty"><GitBranch size={22} /><strong>尚未添加扩展源</strong><button className="btn btn-primary" disabled={loading} onClick={() => void addOfficialSource()}><Plus size={15} />添加 HiMind 扩展源</button></div> : null}
        </div>
      </div>
    </div>
  </div>;
}

function messageOf(reason: unknown) {
  return reason instanceof Error ? reason.message : String(reason || '扩展源操作失败');
}

function workspaceName(root: string) {
  const normalized = root.replace(/[\\/]+$/, '');
  return normalized.split(/[\\/]/).pop() || '本地开发仓库';
}
