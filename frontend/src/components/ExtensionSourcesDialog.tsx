import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { Check, ChevronDown, CircleAlert, FolderCheck, FolderOpen, GitBranch, Info, MessageCircle, MoreHorizontal, Plus, RefreshCw, ShieldCheck, Trash2, X } from 'lucide-react';
import { agentApi, type ExtensionSourceConfig, type ExtensionSourceSettings, type ExtensionSourceSnapshot, type ExtensionSourceStatus, type ExtensionWorkspaceSettings } from '../services/agentApi';

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
  const officialAdded = githubSources.some(source => normalizePath(source.repository) === 'mrbaoquan/himind-extensions');

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
      <div className="modal-header extension-source-header">
        <h3 id="extension-source-title">扩展源</h3>
        <div className="actions-row">
          <Menu variant="primary" label="添加来源" icon={<Plus size={15} />} disabled={loading}>
            {close => <>
              <MenuItem icon={<FolderOpen size={14} />} label="本地开发工作区" disabled={loading} onClick={() => { close(); openForm('local'); }} />
              <MenuItem icon={<GitBranch size={14} />} label="GitHub 分发源" disabled={loading} onClick={() => { close(); openForm('github'); }} />
              <div className="app-menu-separator" />
              <MenuItem icon={<ShieldCheck size={14} />} label="HiMind 官方扩展源" title={officialAdded ? '已添加 HiMind 官方扩展源' : 'MrBaoquan/himind-extensions'} disabled={loading || officialAdded} onClick={() => { close(); void addOfficialSource(); }} />
            </>}
          </Menu>
          <button className="btn btn-icon" title="刷新扩展源" aria-label="刷新扩展源" disabled={loading} onClick={() => void onRefresh().catch(reason => setLocalError(messageOf(reason)))}><RefreshCw className={loading ? 'spin' : ''} size={15} /></button>
          <button className="btn btn-icon" title="关闭" aria-label="关闭" onClick={onClose}><X size={15} /></button>
        </div>
      </div>
      <div className="modal-body extension-source-body">
        {formOpen ? <section className="extension-source-form" aria-label={sourceType === 'local' ? '添加本地开发工作区' : '添加 GitHub 分发源'}>
          <div className="extension-source-form-head">
            <strong>{sourceType === 'local' ? '添加本地开发工作区' : '添加 GitHub 分发源'}</strong>
            <span>{sourceType === 'local' ? '选择包含 extensions.json 聚合清单的本地目录，制品与清单均从本地读取' : '按仓库地址安装并分发扩展'}</span>
          </div>
          <div className="extension-source-form-grid">
            {sourceType === 'local'
              ? <div className="field-group extension-source-form-wide"><label className="field-label" htmlFor="extension-source-local-root">本地聚合目录</label><div className="extension-source-local-row"><input id="extension-source-local-root" value={localRoot} onChange={event => setLocalRoot(event.target.value)} placeholder="F:\WebProjects\himind-extensions" /><button className="btn btn-icon" title="选择本地聚合目录" aria-label="选择本地聚合目录" disabled={loading} onClick={() => void pickLocalRoot()}><FolderOpen size={15} /></button></div></div>
              : <div className="field-group extension-source-form-wide"><label className="field-label" htmlFor="extension-source-repository">GitHub 仓库链接</label><input id="extension-source-repository" value={repository} onChange={event => setRepository(event.target.value)} placeholder="https://github.com/owner/repository" /></div>}
            <div className="field-group"><label className="field-label" htmlFor="extension-source-name">名称（可选）</label><input id="extension-source-name" value={name} onChange={event => setName(event.target.value)} placeholder={sourceType === 'local' ? '默认使用目录名' : '默认使用仓库名'} /></div>
            {sourceType === 'local'
              ? <div className="field-group"><label className="field-label" htmlFor="extension-source-catalog">目录文件</label><input id="extension-source-catalog" value={catalogPath} onChange={event => setCatalogPath(event.target.value)} placeholder="extensions.json" /></div>
              : <div className="field-group"><label className="field-label" htmlFor="extension-source-reference">分支或 Tag</label><input id="extension-source-reference" value={reference} onChange={event => setReference(event.target.value)} placeholder="main" /></div>}
            {sourceType === 'github' ? <>
              <div className="field-group"><label className="field-label" htmlFor="extension-source-catalog">目录文件</label><input id="extension-source-catalog" value={catalogPath} onChange={event => setCatalogPath(event.target.value)} placeholder=".himind/catalog.json" /></div>
              <div className="field-group"><label className="field-label" htmlFor="extension-source-verification">来源校验</label><select id="extension-source-verification" value={verification} onChange={event => setVerification(event.target.value as ExtensionSourceConfig['verification'])}><option value="required">仅安装可信签名</option><option value="optional">允许用户自定义制品</option></select></div>
              <small className="extension-source-form-wide extension-source-form-hint">选择用户自定义时，已有签名仍会严格校验。</small>
            </> : <small className="extension-source-form-wide extension-source-form-hint">本地目录源固定使用用户自定义制品校验，不要求签名。</small>}
          </div>
          <div className="extension-source-form-actions"><button className="btn" onClick={() => setFormOpen(false)}>取消</button><button className="btn btn-primary" disabled={loading || (sourceType === 'github' ? (!repository.trim() || !reference.trim() || !catalogPath.trim()) : (!localRoot.trim() || !catalogPath.trim()))} onClick={() => void addSource()}>保存</button></div>
        </section> : null}

        {(localError || error) ? <div className="skill-inline-warning"><CircleAlert size={15} /><span>{localError || error}</span></div> : null}

        <div className="extension-source-columns" aria-hidden="true"><span>来源 · {settings.sources.length + (boundRoot ? 1 : 0)}</span><span>状态</span><span>操作</span></div>

        <div className="extension-source-list">
          {boundRoot ? <SourceRow
            active
            kind="workspace"
            title={baseName(boundRoot)}
            meta={boundRoot}
            dot="success"
            statusText="已绑定"
            countText="未登记为本地源"
            actions={<>
              <span className="extension-source-slot" />
              <Menu label="当前工作区更多操作" icon={<MoreHorizontal size={15} />} disabled={loading}>
                {close => <>
                  <MenuItem icon={<FolderCheck size={14} />} label="登记为本地源" disabled={loading} onClick={() => { close(); void registerWorkspace(boundRoot); }} />
                  <MenuItem icon={<MessageCircle size={14} />} label="用 AI 开发" disabled={loading} onClick={() => { close(); onDevelopWorkspace(boundRoot); }} />
                </>}
              </Menu>
            </>}
          /> : null}
          {localSources.map(source => {
            const status = statuses.get(source.id);
            const ready = status?.state === 'ready';
            const active = activeRoot !== '' && normalizePath(source.repository) === activeRoot;
            const upstream = (source.upstream_repository || '').trim();
            const upstreamAdded = Boolean(upstream) && githubSources.some(item => normalizePath(item.repository) === normalizePath(upstream));
            const label = displayName(source);
            return <SourceRow
              key={source.id}
              active={active}
              kind="workspace"
              title={label}
              meta={source.repository}
              upstream={upstream}
              upstreamAdded={upstreamAdded}
              dot={ready ? 'success' : source.enabled ? 'danger' : ''}
              statusText={statusLabel(source.enabled, status)}
              countText={status ? `${status.plugin_count} 插件 · ${status.skill_count} 技能` : ''}
              actions={<>
                <SourceSwitch title="启用该来源" label={`启用 ${label}`} checked={source.enabled} disabled={loading} onChange={value => void updateSource(source, value, source.auto_update, source.verification)} />
                <Menu label={`${label} 更多操作`} icon={<MoreHorizontal size={15} />} disabled={loading}>
                  {close => <>
                    <MenuItem icon={<MessageCircle size={14} />} label="用 AI 开发" disabled={loading} onClick={() => { close(); void developWorkspace(source); }} />
                    {active ? null : <MenuItem icon={<FolderCheck size={14} />} label="设为当前工作区" disabled={loading} onClick={() => { close(); void setActiveWorkspace(source); }} />}
                    {upstream && !upstreamAdded ? <MenuItem icon={<GitBranch size={14} />} label="添加为分发源" title={upstream} disabled={loading} onClick={() => { close(); prefillUpstreamSource(source); }} /> : null}
                    <div className="app-menu-separator" />
                    <MenuItem danger icon={<Trash2 size={14} />} label="移除" disabled={loading} onClick={() => { close(); void removeSource(source.id); }} />
                  </>}
                </Menu>
              </>}
              messages={<>
                {source.enabled && status?.error ? <SourceMessage kind="error" text={status.error} /> : null}
                {source.enabled && status?.notice ? <SourceMessage kind="notice" text={status.notice} /> : null}
              </>}
            />;
          })}
          {githubSources.map(source => {
            const status = statuses.get(source.id);
            const ready = status?.state === 'ready';
            const official = normalizePath(source.repository) === 'mrbaoquan/himind-extensions';
            const label = displayName(source);
            return <SourceRow
              key={source.id}
              kind="distribution"
              title={label}
              meta={source.repository}
              reference={source.reference}
              dot={ready ? 'success' : source.enabled ? 'danger' : ''}
              statusText={statusLabel(source.enabled, status)}
              countText={status ? `${status.plugin_count} 插件 · ${status.skill_count} 技能` : ''}
              actions={<>
                <SourceSwitch title="启用该来源" label={`启用 ${label}`} checked={source.enabled} disabled={loading} onChange={value => void updateSource(source, value, source.auto_update, source.verification)} />
                <Menu label={`${label} 更多操作`} icon={<MoreHorizontal size={15} />} disabled={loading}>
                  {close => <>
                    <MenuItem checked={source.auto_update} label="自动更新" disabled={loading || !source.enabled} onClick={() => { close(); void updateSource(source, source.enabled, !source.auto_update, source.verification); }} />
                    <div className="app-menu-separator" />
                    <MenuItem checked={source.verification === 'required'} label="仅安装可信签名" title={official ? 'HiMind 官方源固定使用可信签名' : undefined} disabled={loading || official} onClick={() => { close(); void updateSource(source, source.enabled, source.auto_update, 'required'); }} />
                    <MenuItem checked={source.verification === 'optional'} label="允许用户自定义制品" title={official ? 'HiMind 官方源固定使用可信签名' : undefined} disabled={loading || official} onClick={() => { close(); void updateSource(source, source.enabled, source.auto_update, 'optional'); }} />
                    <div className="app-menu-separator" />
                    <MenuItem danger icon={<Trash2 size={14} />} label="移除" disabled={loading} onClick={() => { close(); void removeSource(source.id); }} />
                  </>}
                </Menu>
              </>}
              messages={<>
                {source.enabled && status?.error ? <SourceMessage kind="error" text={status.error} /> : null}
                {source.enabled && status?.notice ? <SourceMessage kind="notice" text={status.notice} /> : null}
              </>}
            />;
          })}
          {!settings.sources.length && !boundRoot ? <div className="extension-source-empty"><FolderOpen size={18} /><strong>尚未添加扩展源</strong><small>用右上角「添加来源」登记本地开发工作区，或用 GitHub 仓库地址安装分发源。</small></div> : null}
        </div>
      </div>
    </div>
  </div>;
}

function SourceRow({ active, kind, title, meta, reference, upstream, upstreamAdded, dot, statusText, countText, actions, messages }: {
  active?: boolean;
  kind: 'workspace' | 'distribution';
  title: string;
  meta: string;
  reference?: string;
  upstream?: string;
  upstreamAdded?: boolean;
  dot: '' | 'success' | 'danger';
  statusText: string;
  countText: string;
  actions: ReactNode;
  messages?: ReactNode;
}) {
  const upstreamTitle = upstream ? (upstreamAdded ? `已添加为分发源：${upstream}` : `上游仓库 ${upstream}，可用「添加为分发源」按仓库地址安装`) : '未识别上游仓库，可在 extensions.json 声明 repository 或配置 git remote origin';
  return <article className={`extension-source-item${active ? ' is-active' : ''}`}>
    <div className="extension-source-item-main">
      <span className="extension-source-mark">{kind === 'workspace' ? <FolderOpen size={15} /> : <GitBranch size={15} />}</span>
      <div className="extension-source-item-text">
        <div className="extension-source-item-title">
          <strong title={title}>{title}</strong>
          <span className="extension-source-kind">{kind === 'workspace' ? '工作区' : '分发源'}</span>
          {active ? <span className="extension-source-tag">当前工作区</span> : null}
        </div>
        <div className="extension-source-item-meta">
          <code title={meta}>{meta}</code>
          {reference ? <span className="extension-source-ref" title={`分支或 Tag ${reference}`}>{reference}</span> : null}
          {upstream === undefined ? null : <span className={`extension-source-upstream${upstreamAdded ? ' is-added' : ''}${upstream ? '' : ' is-missing'}`} title={upstreamTitle}><GitBranch size={11} /><span>{upstream || '未识别上游仓库'}</span></span>}
        </div>
      </div>
    </div>
    <div className="extension-source-item-status">
      <span><span className={`status-dot ${dot}`} />{statusText}</span>
      <small>{countText}</small>
    </div>
    <div className="extension-source-item-actions">{actions}</div>
    {messages}
  </article>;
}

function Menu({ label, icon, variant = 'icon', disabled, children }: {
  label: string;
  icon: ReactNode;
  variant?: 'icon' | 'primary';
  disabled?: boolean;
  children: (close: () => void) => ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState({ top: 0, left: 0 });
  const anchor = useRef<HTMLDivElement | null>(null);
  const place = useCallback(() => {
    const rect = anchor.current?.getBoundingClientRect();
    if (!rect) return;
    const left = Math.max(8, Math.min(rect.right - MENU_WIDTH, window.innerWidth - MENU_WIDTH - 8));
    const below = rect.bottom + 6;
    const top = below + MENU_MAX_HEIGHT > window.innerHeight - 8 ? Math.max(8, rect.top - 6 - MENU_MAX_HEIGHT) : below;
    setPosition({ top, left });
  }, []);

  useEffect(() => {
    if (!open) return;
    function dismiss(event: MouseEvent) {
      if (anchor.current && !anchor.current.contains(event.target as Node)) setOpen(false);
    }
    function onKey(event: KeyboardEvent) {
      if (event.key === 'Escape') setOpen(false);
    }
    place();
    document.addEventListener('mousedown', dismiss);
    document.addEventListener('keydown', onKey);
    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    return () => {
      document.removeEventListener('mousedown', dismiss);
      document.removeEventListener('keydown', onKey);
      window.removeEventListener('resize', place);
      window.removeEventListener('scroll', place, true);
    };
  }, [open, place]);

  return <div className="extension-source-menu" ref={anchor}>
    <button type="button" className={variant === 'primary' ? 'btn btn-primary' : `btn btn-icon${open ? ' is-open' : ''}`} title={label} aria-label={label} aria-haspopup="menu" aria-expanded={open} disabled={disabled} onClick={() => { place(); setOpen(value => !value); }}>
      {icon}
      {variant === 'primary' ? <><span>{label}</span><ChevronDown size={13} /></> : null}
    </button>
    {open ? <div className="app-menu-dropdown extension-source-menu-panel" role="menu" style={{ top: position.top, left: position.left }}>{children(() => setOpen(false))}</div> : null}
  </div>;
}

function MenuItem({ icon, label, title, checked, danger, disabled, onClick }: {
  icon?: ReactNode;
  label: string;
  title?: string;
  checked?: boolean;
  danger?: boolean;
  disabled?: boolean;
  onClick: () => void;
}) {
  return <button type="button" role="menuitem" className={danger ? 'danger' : undefined} title={title} disabled={disabled} onClick={onClick}>
    {checked === undefined ? (icon ?? <span className="extension-source-menu-blank" />) : checked ? <Check size={14} /> : <span className="extension-source-menu-blank" />}
    <span>{label}</span>
  </button>;
}

function baseName(value: string) {
  const trimmed = value.replace(/[\\/]+$/, '');
  return trimmed.split(/[\\/]/).pop() || trimmed;
}

function displayName(source: ExtensionSourceConfig) {
  const name = (source.name || '').trim();
  if (name && normalizePath(name) !== normalizePath(source.repository)) return name;
  return baseName(source.repository);
}

function statusLabel(enabled: boolean, status?: ExtensionSourceStatus) {
  if (!enabled) return '已停用';
  if (status?.state === 'ready') return status.using_cache ? '缓存可用' : '可用';
  return status ? '不可用' : '待刷新';
}

function SourceSwitch({ title, label, checked, disabled, onChange }: { title: string; label: string; checked: boolean; disabled: boolean; onChange: (value: boolean) => void }) {
  return <label className="toggle compact" title={title}>
    <input type="checkbox" checked={checked} disabled={disabled} aria-label={label} onChange={event => onChange(event.target.checked)} />
    <span className="slider" />
  </label>;
}

function SourceMessage({ kind, text }: { kind: 'error' | 'notice'; text: string }) {
  const [expanded, setExpanded] = useState(false);
  const Icon = kind === 'error' ? CircleAlert : Info;
  return <button type="button" className={`extension-source-message is-${kind}${expanded ? ' is-expanded' : ''}`} aria-expanded={expanded} title={expanded ? '点击收起' : text} onClick={() => setExpanded(value => !value)}>
    <Icon size={13} />
    <span>{text}</span>
    <ChevronDown className="extension-source-message-caret" size={13} />
  </button>;
}

function messageOf(reason: unknown) {
  return reason instanceof Error ? reason.message : String(reason || '扩展源操作失败');
}

function normalizePath(value: string) {
  return value.trim().replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();
}

const MENU_WIDTH = 208;
const MENU_MAX_HEIGHT = 264;
