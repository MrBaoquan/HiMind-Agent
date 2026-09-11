import { useEffect, useMemo, useState } from 'react';
import { CircleAlert, FolderOpen, GitBranch, MessageCircle, Plus, RefreshCw, Trash2, X } from 'lucide-react';
import { agentApi, type ExtensionSourceConfig, type ExtensionSourceSettings, type ExtensionSourceSnapshot, type ExtensionWorkspaceSettings } from '../services/agentApi';

type Props = {
  open: boolean;
  workspace: ExtensionWorkspaceSettings;
  settings: ExtensionSourceSettings;
  snapshot: ExtensionSourceSnapshot | null;
  loading: boolean;
  error: string;
  onClose: () => void;
  onSetWorkspace: (root: string) => Promise<void>;
  onDevelopWorkspace: (root: string) => void;
  onRefresh: () => Promise<void>;
  onAdd: (name: string, repository: string, reference: string, catalogPath: string, verification: ExtensionSourceConfig['verification']) => Promise<void>;
  onAddLocal: (name: string, root: string, catalogPath?: string) => Promise<void>;
  onUpdate: (source: ExtensionSourceConfig, enabled: boolean, autoUpdate: boolean, verification: ExtensionSourceConfig['verification']) => Promise<void>;
  onRemove: (sourceId: string) => Promise<void>;
};

export function ExtensionSourcesDialog({ open, workspace, settings, snapshot, loading, error, onClose, onSetWorkspace, onDevelopWorkspace, onRefresh, onAdd, onAddLocal, onUpdate, onRemove }: Props) {
  const [sourceType, setSourceType] = useState<'github' | 'local'>('github');
  const [repository, setRepository] = useState('');
  const [name, setName] = useState('');
  const [reference, setReference] = useState('main');
  const [catalogPath, setCatalogPath] = useState('.himind/catalog.json');
  const [localRoot, setLocalRoot] = useState('');
  const [verification, setVerification] = useState<ExtensionSourceConfig['verification']>('required');
  const [formOpen, setFormOpen] = useState(false);
  const [localError, setLocalError] = useState('');
  const statuses = useMemo(() => new Map((snapshot?.sources || []).map(item => [item.source.id, item])), [snapshot]);
  const localSources = useMemo(() => settings.sources.filter(source => source.kind === 'local'), [settings.sources]);
  const githubSources = useMemo(() => settings.sources.filter(source => source.kind !== 'local'), [settings.sources]);
  const activeRoot = workspace.valid ? normalizePath(workspace.root) : '';
  const boundRoot = activeRoot !== '' && !localSources.some(source => normalizePath(source.repository) === activeRoot) ? workspace.root : '';

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
      if (sourceType === 'local') {
        await onAddLocal(name.trim(), localRoot.trim(), catalogPath.trim());
      } else {
        await onAdd(name.trim(), repository.trim(), reference.trim(), catalogPath.trim(), verification);
      }
      setName('');
      setRepository('');
      setReference('main');
      setCatalogPath(sourceType === 'local' ? 'extensions.json' : '.himind/catalog.json');
      setLocalRoot('');
      setVerification('required');
      setFormOpen(false);
      setSourceType('github');
    } catch (reason) {
      setLocalError(messageOf(reason));
    }
  }

  function openForm(type: 'github' | 'local') {
    setSourceType(type);
    setCatalogPath(type === 'local' ? 'extensions.json' : '.himind/catalog.json');
    setFormOpen(true);
  }

  async function setActiveWorkspace(source: ExtensionSourceConfig) {
    setLocalError('');
    try { await onSetWorkspace(source.repository); }
    catch (reason) { setLocalError(messageOf(reason)); }
  }

  async function developWorkspace(source: ExtensionSourceConfig) {
    setLocalError('');
    try {
      if (normalizePath(source.repository) !== activeRoot) await onSetWorkspace(source.repository);
      onDevelopWorkspace(source.repository);
    } catch (reason) { setLocalError(messageOf(reason)); }
  }

  async function registerWorkspace(root: string) {
    setLocalError('');
    try { await onAddLocal('', root); }
    catch (reason) { setLocalError(messageOf(reason)); }
  }

  function prefillUpstreamSource(source: ExtensionSourceConfig) {
    setLocalError('');
    setSourceType('github');
    setRepository((source.upstream_repository || '').trim());
    setName(source.name);
    setReference('main');
    setCatalogPath('.himind/catalog.json');
    setFormOpen(true);
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

  async function pickLocalRoot() {
    setLocalError('');
    try {
      const picked = await agentApi.pickLocalExtensionSourceDir();
      if (picked) setLocalRoot(picked);
    } catch (reason) {
      setLocalError(messageOf(reason));
    }
  }

  return <div className="modal-backdrop extension-source-backdrop" role="presentation">
    <div className="modal extension-source-dialog" role="dialog" aria-modal="true" aria-labelledby="extension-source-title">
      <div className="modal-header">
        <div><h3 id="extension-source-title">扩展源</h3><p>本地开发工作区与 GitHub 分发源</p></div>
        <div className="actions-row">
          <button className="btn btn-icon" title="刷新扩展源" aria-label="刷新扩展源" disabled={loading} onClick={() => void onRefresh().catch(reason => setLocalError(messageOf(reason)))}><RefreshCw className={loading ? 'spin' : ''} size={16} /></button>
          <button className="btn btn-icon" title="关闭" aria-label="关闭" onClick={onClose}><X size={16} /></button>
        </div>
      </div>
      <div className="modal-body extension-source-body">
        {formOpen ? <section className="extension-source-form">
          <div className="field-group"><label className="field-label" htmlFor="extension-source-type">来源类型</label><select id="extension-source-type" value={sourceType} onChange={event => { const next = event.target.value as 'github' | 'local'; setSourceType(next); setCatalogPath(next === 'local' ? 'extensions.json' : '.himind/catalog.json'); }}><option value="github">GitHub 仓库</option><option value="local">本地目录</option></select></div>
          {sourceType === 'github' ? <div className="field-group"><label className="field-label" htmlFor="extension-source-repository">GitHub 仓库链接</label><input id="extension-source-repository" value={repository} onChange={event => setRepository(event.target.value)} placeholder="https://github.com/owner/repository" /></div> : <div className="field-group"><label className="field-label" htmlFor="extension-source-local-root">本地聚合目录</label><div className="extension-source-local-row"><input id="extension-source-local-root" value={localRoot} onChange={event => setLocalRoot(event.target.value)} placeholder="F:\WebProjects\himind-extensions" /><button className="btn btn-icon" title="选择本地聚合目录" aria-label="选择本地聚合目录" disabled={loading} onClick={() => void pickLocalRoot()}><FolderOpen size={15} /></button></div><small>选择包含 extensions.json 聚合清单的本地目录，catalog 与制品均从本地读取。</small></div>}
          <details className="extension-source-advanced"><summary>高级设置</summary><div className="extension-source-advanced-fields">
            <div className="extension-source-form-row">
              <div className="field-group"><label className="field-label" htmlFor="extension-source-name">名称</label><input id="extension-source-name" value={name} onChange={event => setName(event.target.value)} placeholder="自动使用目录名或仓库名称" /></div>
              {sourceType === 'github' ? <div className="field-group"><label className="field-label" htmlFor="extension-source-reference">分支或 Tag</label><input id="extension-source-reference" value={reference} onChange={event => setReference(event.target.value)} placeholder="main" /></div> : null}
            </div>
            <div className="field-group"><label className="field-label" htmlFor="extension-source-catalog">目录文件</label><input id="extension-source-catalog" value={catalogPath} onChange={event => setCatalogPath(event.target.value)} placeholder={sourceType === 'local' ? 'extensions.json' : '.himind/catalog.json'} /></div>
            {sourceType === 'github' ? <div className="field-group"><label className="field-label" htmlFor="extension-source-verification">来源校验</label><select id="extension-source-verification" value={verification} onChange={event => setVerification(event.target.value as ExtensionSourceConfig['verification'])}><option value="required">仅安装可信签名</option><option value="optional">允许用户自定义制品</option></select><small>选择用户自定义时，已有签名仍会严格校验。</small></div> : <small>本地目录源固定使用用户自定义制品校验，不要求签名。</small>}
          </div></details>
          <div className="extension-source-form-actions"><button className="btn" onClick={() => setFormOpen(false)}>取消</button><button className="btn btn-primary" disabled={loading || (sourceType === 'github' ? (!repository.trim() || !reference.trim() || !catalogPath.trim()) : (!localRoot.trim() || !catalogPath.trim()))} onClick={() => void addSource()}>保存</button></div>
        </section> : null}

        {(localError || error) ? <div className="skill-inline-warning"><CircleAlert size={15} /><span>{localError || error}</span></div> : null}

        <section className="extension-source-group" aria-label="本地开发工作区">
          <div className="extension-source-toolbar">
            <span>本地开发工作区 · {localSources.length}</span>
            <div className="actions-row">
              <button className="btn btn-primary" disabled={loading} onClick={() => openForm('local')}><Plus size={15} />添加工作区</button>
            </div>
          </div>
          <div className="extension-source-list">
            {boundRoot ? <article className="extension-source-item is-workspace is-active">
              <div className="extension-source-item-main">
                <span className="extension-source-mark"><FolderOpen size={17} /></span>
                <div><strong>当前工作区</strong><code title={boundRoot}>{boundRoot}</code><small>已绑定为当前开发工作区，但未登记为本地开发工作区；登记后才会作为扩展源参与刷新与安装。</small></div>
              </div>
              <div className="extension-source-item-status">
                <span><span className="status-dot success" />当前开发工作区</span>
                <small>未登记为本地源</small>
              </div>
              <div className="extension-source-controls">
                <button className="btn btn-primary" disabled={loading} onClick={() => void registerWorkspace(boundRoot)}><Plus size={14} />登记为本地源</button>
                <button className="btn" disabled={loading} onClick={() => onDevelopWorkspace(boundRoot)}><MessageCircle size={14} />用 AI 开发</button>
              </div>
            </article> : null}
            {localSources.map(source => {
              const status = statuses.get(source.id);
              const ready = status?.state === 'ready';
              const active = activeRoot !== '' && normalizePath(source.repository) === activeRoot;
              const upstream = (source.upstream_repository || '').trim();
              const upstreamAdded = Boolean(upstream) && githubSources.some(item => normalizePath(item.repository) === normalizePath(upstream));
              return <article className={`extension-source-item is-workspace${active ? ' is-active' : ''}`} key={source.id}>
                <div className="extension-source-item-main">
                  <span className="extension-source-mark"><FolderOpen size={17} /></span>
                  <div><strong>{source.name || source.repository}</strong><code title={source.repository}>{source.repository}</code><small>{upstream ? `上游仓库 ${upstream}` : '未识别上游仓库，可在 extensions.json 声明 repository 或配置 git remote'} · {source.catalog_path}</small></div>
                </div>
                <div className="extension-source-item-status">
                  <span><span className={`status-dot ${ready ? 'success' : source.enabled ? 'danger' : ''}`} />{!source.enabled ? '已停用' : ready ? (status?.using_cache ? '缓存可用' : '可用') : status ? '不可用' : '待刷新'}</span>
                  {status ? <small>{status.plugin_count} 个插件 · {status.skill_count} 个技能</small> : null}
                  <small>{active ? '当前开发工作区' : '本地用户自定义来源'}</small>
                </div>
                <div className="extension-source-controls">
                  <label><span>启用</span><span className="toggle"><input type="checkbox" checked={source.enabled} disabled={loading} onChange={event => void updateSource(source, event.target.checked, source.auto_update, source.verification)} /><span className="slider" /></span></label>
                  {!active ? <button className="btn" disabled={loading} onClick={() => void setActiveWorkspace(source)}><FolderOpen size={14} />设为工作区</button> : null}
                  <button className="btn btn-primary" disabled={loading} onClick={() => void developWorkspace(source)}><MessageCircle size={14} />用 AI 开发</button>
                  {upstream && !upstreamAdded ? <button className="btn" disabled={loading} title={`用 ${upstream} 添加 GitHub 分发源`} onClick={() => prefillUpstreamSource(source)}><GitBranch size={14} />添加为分发源</button> : null}
                  <button className="btn btn-icon btn-danger-quiet" title="移除本地工作区" aria-label={`移除 ${source.name || source.repository}`} disabled={loading} onClick={() => void removeSource(source.id)}><Trash2 size={15} /></button>
                </div>
                {source.enabled && status?.error ? <div className="extension-source-item-error">{status.error}</div> : null}
                {source.enabled && status?.notice ? <div className="extension-source-item-notice">{status.notice}</div> : null}
              </article>;
            })}
            {!localSources.length && !boundRoot ? <div className="extension-source-empty"><FolderOpen size={22} /><strong>尚未添加本地开发工作区</strong><small>选择包含 extensions.json 聚合清单的本地目录，即可在本机开发、构建并验证扩展。</small></div> : null}
          </div>
        </section>

        <section className="extension-source-group" aria-label="GitHub 分发源">
          <div className="extension-source-toolbar">
            <span>GitHub 分发源 · {githubSources.length}</span>
            <div className="actions-row">
              <button className="btn btn-primary" disabled={loading} onClick={() => openForm('github')}><Plus size={15} />添加源</button>
            </div>
          </div>
          <div className="extension-source-list">
            {githubSources.map(source => {
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
                {source.enabled && status?.notice ? <div className="extension-source-item-notice">{status.notice}</div> : null}
              </article>;
            })}
            {!githubSources.length ? <div className="extension-source-empty"><GitBranch size={22} /><strong>尚未添加 GitHub 分发源</strong><small>本地工作区验证通过并推送到 GitHub 后，在此用仓库地址安装分发。</small><button className="btn btn-primary" disabled={loading} onClick={() => void addOfficialSource()}><Plus size={15} />添加 HiMind 扩展源</button></div> : null}
          </div>
        </section>
      </div>
    </div>
  </div>;
}

function messageOf(reason: unknown) {
  return reason instanceof Error ? reason.message : String(reason || '扩展源操作失败');
}

function normalizePath(value: string) {
  return value.trim().replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();
}
