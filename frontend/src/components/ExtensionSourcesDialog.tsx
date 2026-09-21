import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { ChevronDown, CircleAlert, Download, FolderCheck, FolderOpen, GitBranch, Info, MessageCircle, Plus, RefreshCw, ShieldAlert, ShieldCheck, Trash2, X } from 'lucide-react';
import { agentApi, type ExtensionDistributionUnit, type ExtensionSourceAcquisition, type ExtensionSourceConfig, type ExtensionSourceNotice, type ExtensionSourceSettings, type ExtensionSourceSnapshot, type ExtensionSourceStatus, type ExtensionWorkspaceSettings } from '../services/agentApi';

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
  onSetAcquisition: (unitKey: string, acquisition: ExtensionSourceAcquisition) => Promise<void>;
  onInstallUnit: (unitKey: string, sourceId: string) => Promise<void>;
};

/// 一个分发单元 = 同一份扩展内容的本地开发工作区与 GitHub 分发源。
type UnitGroup = {
  key: string;
  unit?: ExtensionDistributionUnit;
  local?: ExtensionSourceConfig;
  remote?: ExtensionSourceConfig;
};

type UnitHandlers = {
  loading: boolean;
  activeRoot: string;
  statuses: Map<string, ExtensionSourceStatus>;
  onSetAcquisition: (unitKey: string, acquisition: ExtensionSourceAcquisition) => Promise<void>;
  onInstallUnit: (unitKey: string, sourceId: string) => Promise<void>;
  onUpdate: (source: ExtensionSourceConfig, enabled: boolean, autoUpdate: boolean, verification: ExtensionSourceConfig['verification']) => Promise<void>;
  onRemove: (sourceId: string) => Promise<void>;
  onSetWorkspace: (source: ExtensionSourceConfig) => Promise<void>;
  onDevelopWorkspace: (source: ExtensionSourceConfig) => Promise<void>;
  onPrefillUpstream: (source: ExtensionSourceConfig) => void;
};

export function ExtensionSourcesDialog({ open, workspace, settings, snapshot, loading, error, onClose, onSetWorkspace, onDevelopWorkspace, onRefresh, onAdd, onAddLocal, onUpdate, onRemove, onSetAcquisition, onInstallUnit }: Props) {
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
  // Do not render the persisted sources as independent rows while the first
  // snapshot is still loading.  The snapshot is what proves that a local
  // directory and its GitHub repository belong to the same extension unit;
  // rendering settings alone briefly makes one unit look like two sources.
  const units = useMemo(() => snapshot ? buildUnitGroups(snapshot, settings) : [], [snapshot, settings]);
  const activeRoot = workspace.valid ? normalizePath(workspace.root) : '';
  const boundRoot = activeRoot !== '' && !settings.sources.some(source => source.kind === 'local' && normalizePath(source.repository) === activeRoot) ? workspace.root : '';
  const officialAdded = settings.sources.some(source => source.kind !== 'local' && normalizePath(source.repository) === 'mrbaoquan/himind-extensions');

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

  async function switchAcquisition(unitKey: string, acquisition: ExtensionSourceAcquisition) {
    setLocalError('');
    try { await onSetAcquisition(unitKey, acquisition); }
    catch (reason) { setLocalError(messageOf(reason)); }
  }

  async function installUnit(unitKey: string, sourceId: string) {
    setLocalError('');
    try { await onInstallUnit(unitKey, sourceId); }
    catch (reason) { setLocalError(messageOf(reason)); }
  }

  const handlers: UnitHandlers = {
    loading,
    activeRoot,
    statuses,
    onSetAcquisition: switchAcquisition,
    onInstallUnit: installUnit,
    onUpdate: updateSource,
    onRemove: removeSource,
    onSetWorkspace: setActiveWorkspace,
    onDevelopWorkspace: developWorkspace,
    onPrefillUpstream: prefillUpstreamSource,
  };

  return <div className="modal-backdrop extension-source-backdrop" role="presentation">
    <div className="modal extension-source-dialog" role="dialog" aria-modal="true" aria-labelledby="extension-source-title">
      <div className="modal-header extension-source-header">
        <h3 id="extension-source-title">来源管理</h3>
        <div className="actions-row">
          <Menu variant="primary" label="添加来源" icon={<Plus size={15} />} disabled={loading}>
            {close => <>
              <MenuItem icon={<FolderOpen size={14} />} label="本地开发目录" disabled={loading} onClick={() => { close(); openForm('local'); }} />
              <MenuItem icon={<GitBranch size={14} />} label="GitHub 仓库" disabled={loading} onClick={() => { close(); openForm('github'); }} />
              <div className="app-menu-separator" />
              <MenuItem icon={<ShieldCheck size={14} />} label="HiMind 官方来源" title={officialAdded ? '已添加 HiMind 官方来源' : 'MrBaoquan/himind-extensions'} disabled={loading || officialAdded} onClick={() => { close(); void addOfficialSource(); }} />
            </>}
          </Menu>
          <button className="btn btn-icon" title="刷新来源" aria-label="刷新来源" disabled={loading} onClick={() => void onRefresh().catch(reason => setLocalError(messageOf(reason)))}><RefreshCw className={loading ? 'spin' : ''} size={15} /></button>
          <button className="btn btn-icon" title="关闭" aria-label="关闭" onClick={onClose}><X size={15} /></button>
        </div>
      </div>
      <div className="modal-body extension-source-body">
        {formOpen ? <section className="extension-source-form" aria-label={sourceType === 'local' ? '添加本地开发目录' : '添加 GitHub 仓库'}>
          <div className="extension-source-form-head">
            <strong>{sourceType === 'local' ? '添加本地开发目录' : '添加 GitHub 仓库'}</strong>
            <span>{sourceType === 'local' ? '选择包含扩展清单的开发目录' : '从仓库读取已发布版本'}</span>
          </div>
          <div className="extension-source-form-grid">
            {sourceType === 'local'
              ? <div className="field-group extension-source-form-wide"><label className="field-label" htmlFor="extension-source-local-root">本地目录</label><div className="extension-source-local-row"><input id="extension-source-local-root" value={localRoot} onChange={event => setLocalRoot(event.target.value)} placeholder="F:\WebProjects\himind-extensions" /><button className="btn btn-icon" title="选择本地目录" aria-label="选择本地目录" disabled={loading} onClick={() => void pickLocalRoot()}><FolderOpen size={15} /></button></div></div>
              : <div className="field-group extension-source-form-wide"><label className="field-label" htmlFor="extension-source-repository">GitHub 仓库链接</label><input id="extension-source-repository" value={repository} onChange={event => setRepository(event.target.value)} placeholder="https://github.com/owner/repository" /></div>}
            <div className="field-group"><label className="field-label" htmlFor="extension-source-name">名称（可选）</label><input id="extension-source-name" value={name} onChange={event => setName(event.target.value)} placeholder={sourceType === 'local' ? '默认使用目录名' : '默认使用仓库名'} /></div>
            {sourceType === 'local'
              ? <div className="field-group"><label className="field-label" htmlFor="extension-source-catalog">清单文件</label><input id="extension-source-catalog" value={catalogPath} onChange={event => setCatalogPath(event.target.value)} placeholder="extensions.json" /></div>
              : <div className="field-group"><label className="field-label" htmlFor="extension-source-reference">分支或版本</label><input id="extension-source-reference" value={reference} onChange={event => setReference(event.target.value)} placeholder="main" /></div>}
            {sourceType === 'github' ? <>
              <div className="field-group"><label className="field-label" htmlFor="extension-source-catalog">清单文件</label><input id="extension-source-catalog" value={catalogPath} onChange={event => setCatalogPath(event.target.value)} placeholder=".himind/catalog.json" /></div>
              <div className="field-group"><label className="field-label" htmlFor="extension-source-verification">签名校验</label><select id="extension-source-verification" value={verification} onChange={event => setVerification(event.target.value as ExtensionSourceConfig['verification'])}><option value="required">仅安装已签名版本</option><option value="optional">允许未签名版本</option></select></div>
              <small className="extension-source-form-wide extension-source-form-hint">允许未签名版本时，已有签名仍会严格校验。</small>
            </> : <small className="extension-source-form-wide extension-source-form-hint">本地目录不要求签名。</small>}
          </div>
          <div className="extension-source-form-actions"><button className="btn" onClick={() => setFormOpen(false)}>取消</button><button className="btn btn-primary" disabled={loading || (sourceType === 'github' ? (!repository.trim() || !reference.trim() || !catalogPath.trim()) : (!localRoot.trim() || !catalogPath.trim()))} onClick={() => void addSource()}>保存</button></div>
        </section> : null}

        {(localError || error) ? <div className="skill-inline-warning"><CircleAlert size={15} /><span>{localError || error}</span></div> : null}

        {/* 列表是卡片而不是表格，所以这里只留分组计数；状态和操作在卡片内自带标签。 */}
        <div className="extension-source-columns" aria-hidden="true"><span>扩展仓库</span><span>{snapshot ? `${units.length + (boundRoot ? 1 : 0)} 个` : '读取中'}</span></div>

        <div className="extension-source-list">
          {boundRoot ? <BoundWorkspaceRow root={boundRoot} loading={loading} onRegister={() => void registerWorkspace(boundRoot)} onDevelop={() => onDevelopWorkspace(boundRoot)} /> : null}
          {units.map(group => <UnitRow key={group.key} group={group} handlers={handlers} />)}
          {!units.length && !boundRoot ? snapshot ? <div className="extension-source-empty"><FolderOpen size={18} /><strong>尚未添加来源</strong><small>用右上角「添加来源」添加本地开发目录或 GitHub 仓库。</small></div> : <div className="extension-source-empty"><RefreshCw className="spin" size={18} /><strong>正在读取来源</strong><small>正在合并本地源码与 GitHub 发布来源。</small></div> : null}
        </div>
      </div>
    </div>
  </div>;
}

function UnitRow({ group, handlers }: { group: UnitGroup; handlers: UnitHandlers }) {
  const { unit, local, remote } = group;
  const status = unitStatus(group, handlers.statuses);
  const install = unitInstallState(group);
  const label = unitTitle(group);
  const active = Boolean(local) && handlers.activeRoot !== '' && normalizePath(local!.repository) === handlers.activeRoot;
  const sourceId = unit ? (unit.acquisition === 'local' ? unit.local_source_id : unit.remote_source_id) : null;
  return <article className={`extension-source-unit${active ? ' is-active' : ''}`}>
    <div className="extension-source-unit-head">
      <div className="extension-source-item-main">
        <span className="extension-source-mark">{local ? <FolderOpen size={15} /> : <GitBranch size={15} />}</span>
        <div className="extension-source-item-text">
          <div className="extension-source-item-title">
            <strong title={label}>{label}</strong>
            {local ? <span className="extension-source-kind">本地源码</span> : null}
            {remote ? <span className="extension-source-kind">GitHub 发布</span> : null}
            {active ? <span className="extension-source-tag">当前工作区</span> : null}
          </div>
          {local && remote ? <div className="extension-source-item-meta"><code title={unit?.repository || remote.repository}>{unit?.repository || remote.repository}</code></div> : null}
        </div>
      </div>
      <div className="extension-source-item-status">
        <span><span className={`status-dot ${status.dot}`} />{status.text}</span>
        <small>{unitSummary(group, install)}</small>
      </div>
      <div className="extension-source-item-actions">
        {local && remote ? <AcquisitionSegment value={unit?.acquisition || 'local'} disabled={handlers.loading} onChange={value => void handlers.onSetAcquisition(unit!.unit_key, value)} /> : null}
        {unit && !local && remote && unit.acquisition === 'local' ? <button type="button" className="btn btn-install" title="切换到 GitHub 发布" disabled={handlers.loading} onClick={() => void handlers.onSetAcquisition(unit.unit_key, 'remote')}><GitBranch size={13} />使用 GitHub 发布</button> : null}
        {unit ? <button className="btn btn-install" title={installTitle(group, install)} disabled={handlers.loading || unit.state !== 'ready' || !sourceId} onClick={() => sourceId && void handlers.onInstallUnit(unit.unit_key, sourceId)}><Download size={14} />{installLabel(install)}</button> : <span className="extension-source-slot" />}
      </div>
    </div>
    <div className="extension-source-unit-members">
      {local ? <MemberRow source={local} kind="local" handlers={handlers} active={active} upstreamAdded={Boolean(remote) || hasUpstreamSource(local, handlers)} /> : null}
      {remote ? <MemberRow source={remote} kind="remote" handlers={handlers} active={false} /> : null}
    </div>
    {unit?.other_side?.available && unit.other_side.newer_count > 0 ? <p className="extension-source-other-side"><Download size={12} /><span>{otherSideLabel(unit.other_side.side)}有 {unit.other_side.newer_count} 项更高版本，切换取用侧即可安装</span></p> : null}
    {install.mismatch ? <p className="extension-source-consistency"><CircleAlert size={12} /><span>本机安装来自另一来源。重新安装后将切换到当前来源。</span></p> : null}
  </article>;
}

function MemberRow({ source, kind, handlers, active, upstreamAdded }: { source: ExtensionSourceConfig; kind: 'local' | 'remote'; handlers: UnitHandlers; active: boolean; upstreamAdded?: boolean }) {
  const status = handlers.statuses.get(source.id);
  const official = kind === 'remote' && normalizePath(source.repository) === 'mrbaoquan/himind-extensions';
  const upstream = (source.upstream_repository || '').trim();
  const label = displayName(source);
  return <div className="extension-source-member">
    <div className="extension-source-member-main">
      <span className="extension-source-kind">{kind === 'local' ? '本地源码' : 'GitHub 发布'}</span>
      <code title={source.repository}>{source.repository}</code>
      {kind === 'remote' ? <span className="extension-source-ref" title={`分支或版本 ${source.reference}`}>{source.reference}</span> : null}
      {kind === 'local' ? <span className={`extension-source-upstream${upstreamAdded ? ' is-added' : ''}${upstream ? '' : ' is-missing'}`} title={upstreamTitle(upstream, upstreamAdded)}><GitBranch size={11} /><span>{upstream || '未关联 GitHub 仓库'}</span></span> : null}
    </div>
    <div className="extension-source-member-actions">
      <SourceSwitch title="启用该来源" label={`启用 ${label}`} checked={source.enabled} disabled={handlers.loading} onChange={value => void handlers.onUpdate(source, value, source.auto_update, source.verification)} />
      {kind === 'local' ? <>
        <IconAction icon={<MessageCircle size={14} />} label="打开扩展开发" title="设为当前工作区并打开扩展开发" disabled={handlers.loading} onClick={() => void handlers.onDevelopWorkspace(source)} />
        {active ? null : <IconAction icon={<FolderCheck size={14} />} label="设为当前工作区" title="设为当前工作区" disabled={handlers.loading} onClick={() => void handlers.onSetWorkspace(source)} />}
        {upstream && !upstreamAdded ? <IconAction icon={<GitBranch size={14} />} label="添加 GitHub 来源" title={`添加关联仓库 ${upstream}`} disabled={handlers.loading} onClick={() => handlers.onPrefillUpstream(source)} /> : null}
      </> : <>
        <IconAction active={source.auto_update} icon={<RefreshCw size={14} />} label="自动更新" title={source.auto_update ? '自动更新已开启，点击关闭' : '自动更新已关闭，点击开启'} disabled={handlers.loading || !source.enabled} onClick={() => void handlers.onUpdate(source, source.enabled, !source.auto_update, source.verification)} />
        <VerificationSegment value={source.verification} disabled={handlers.loading || official} onChange={value => void handlers.onUpdate(source, source.enabled, source.auto_update, value)} />
      </>}
      <IconAction danger icon={<Trash2 size={14} />} label="移除" title={`移除 ${label}`} disabled={handlers.loading} onClick={() => void handlers.onRemove(source.id)} />
    </div>
    {source.enabled && status?.error ? <SourceError text={status.error} /> : null}
    {source.enabled && status?.notices?.length ? <SourceNotices notices={status.notices} /> : null}
  </div>;
}

function BoundWorkspaceRow({ root, loading, onRegister, onDevelop }: { root: string; loading: boolean; onRegister: () => void; onDevelop: () => void }) {
  return <article className="extension-source-unit is-active">
    <div className="extension-source-unit-head">
      <div className="extension-source-item-main">
        <span className="extension-source-mark"><FolderCheck size={15} /></span>
        <div className="extension-source-item-text">
          <div className="extension-source-item-title">
            <strong title={baseName(root)}>{baseName(root)}</strong>
            <span className="extension-source-kind">本地源码</span>
            <span className="extension-source-tag">当前工作区</span>
          </div>
        </div>
      </div>
      <div className="extension-source-item-status">
        <span><span className="status-dot success" />已关联</span>
        <small>尚未登记为来源</small>
      </div>
      <div className="extension-source-item-actions">
        <span className="extension-source-slot" />
        <IconAction icon={<FolderCheck size={14} />} label="添加为来源" title="把当前工作区添加为来源" disabled={loading} onClick={onRegister} />
        <IconAction icon={<MessageCircle size={14} />} label="打开扩展开发" title="打开当前工作区的扩展开发" disabled={loading} onClick={onDevelop} />
      </div>
    </div>
    <div className="extension-source-unit-members">
      <div className="extension-source-member">
        <div className="extension-source-member-main"><span className="extension-source-kind">本地源码</span><code title={root}>{root}</code></div>
      </div>
    </div>
  </article>;
}

function AcquisitionSegment({ value, disabled, onChange }: { value: ExtensionSourceAcquisition; disabled: boolean; onChange: (value: ExtensionSourceAcquisition) => void }) {
  const options: { key: ExtensionSourceAcquisition; label: string; title: string }[] = [
    { key: 'local', label: '本地源码', title: '从本地工作区安装' },
    { key: 'remote', label: 'GitHub 发布', title: '从 GitHub Release 安装' },
  ];
  return <div className="extension-source-acquisition" role="group" aria-label="安装来源">
    {options.map(option => <button key={option.key} type="button" className={`btn${value === option.key ? ' is-on' : ''}`} title={option.title} aria-pressed={value === option.key} disabled={disabled} onClick={() => onChange(option.key)}>{option.label}</button>)}
  </div>;
}

function buildUnitGroups(snapshot: ExtensionSourceSnapshot | null, settings: ExtensionSourceSettings): UnitGroup[] {
  const byId = new Map(settings.sources.map(source => [source.id, source]));
  const claimed = new Set<string>();
  const groups: UnitGroup[] = [];
  for (const unit of snapshot?.units || []) {
    const local = unit.local_source_id ? byId.get(unit.local_source_id) : undefined;
    const remote = unit.remote_source_id ? byId.get(unit.remote_source_id) : undefined;
    if (local) claimed.add(local.id);
    if (remote) claimed.add(remote.id);
    groups.push({ key: unit.unit_key, unit, local, remote });
  }
  for (const source of settings.sources) {
    if (claimed.has(source.id)) continue;
    groups.push(source.kind === 'local'
      ? { key: `source:${source.id}`, local: source }
      : { key: `source:${source.id}`, remote: source });
  }
  return groups;
}

function unitTitle(group: UnitGroup) {
  const unitName = (group.unit?.name || '').trim();
  if (unitName) return unitName;
  const source = group.local || group.remote;
  return source ? displayName(source) : '来源';
}

function unitStatus(group: UnitGroup, statuses: Map<string, ExtensionSourceStatus>): { dot: '' | 'success' | 'danger'; text: string } {
  if (group.unit?.state === 'unavailable') return { dot: 'danger', text: group.unit.acquisition === 'local' ? '本地源码不可用' : 'GitHub 发布不可用' };
  const members = [group.local, group.remote].filter(Boolean) as ExtensionSourceConfig[];
  const enabled = members.filter(source => source.enabled);
  if (members.length && !enabled.length) return { dot: '', text: '已停用' };
  const ready = enabled.find(source => statuses.get(source.id)?.state === 'ready');
  if (!ready) return { dot: 'danger', text: enabled.some(source => statuses.has(source.id)) ? '不可用' : '待刷新' };
  return { dot: 'success', text: statuses.get(ready.id)?.using_cache ? '缓存可用' : '可用' };
}

function unitInstallState(group: UnitGroup) {
  const unit = group.unit;
  if (!unit) return { installed: 0, updates: 0, missing: 0, mismatch: false };
  const installed = new Map(unit.installed.map(item => [`${item.asset_kind}:${item.asset_id}`, item]));
  let updates = 0;
  let missing = 0;
  let mismatch = false;
  for (const asset of unit.assets) {
    const record = installed.get(`${asset.asset_kind}:${asset.asset_id}`);
    if (!record) {
      missing += 1;
      updates += 1;
      continue;
    }
    const versionChanged = record.version !== asset.version;
    const digestChanged = Boolean(asset.sha256 && record.sha256 && asset.sha256.toLowerCase() !== record.sha256.toLowerCase());
    // 一个制品只计一次。版本和摘要同时变化时仍然只是一个待更新项。
    if (versionChanged || digestChanged) updates += 1;
    // 免安装的开发挂载就是本地工作区的当前内容，按本地侧计。
    if ((record.side === 'development' ? 'local' : record.side) !== unit.acquisition) mismatch = true;
  }
  return { installed: unit.installed.length, updates, missing, mismatch };
}

type UnitInstallState = { installed: number; updates: number; missing: number; mismatch: boolean };

function unitSummary(group: UnitGroup, install: UnitInstallState) {
  const parts: string[] = [];
  if (group.unit) {
    parts.push(group.unit.acquisition === 'local' ? '当前来源：本地源码' : '当前来源：GitHub 发布');
    if (group.unit.state === 'ready') {
      if (group.unit.plugin_count) parts.push(`${group.unit.plugin_count} 插件`);
      if (group.unit.skill_count) parts.push(`${group.unit.skill_count} 技能`);
      if (group.unit.workflow_count) parts.push(`${group.unit.workflow_count} 工作流`);
    }
    if (!parts.length) parts.push('未提供扩展');
  }
  if (install.installed) parts.push(`已安装 ${install.installed} 项`);
  const updates = install.updates - install.missing;
  if (install.missing) parts.push(`${install.missing} 项待安装`);
  if (updates) parts.push(`${updates} 项待更新`);
  return parts.join(' · ');
}

function installLabel(install: UnitInstallState) {
  const missing = install.missing;
  if (missing && install.updates > missing) return '安装或更新';
  if (missing) return '安装';
  if (install.updates) return '更新';
  return install.installed ? '重新安装' : '安装到本机';
}

function installTitle(group: UnitGroup, install: UnitInstallState) {
  const side = group.unit?.acquisition === 'remote' ? 'GitHub 发布' : '本地源码';
  const base = `使用${side}安装插件、技能和工作流`;
  const missing = install.missing;
  const updates = install.updates - missing;
  if (missing && updates) return `${base}，${missing} 项待安装、${updates} 项待更新`;
  if (missing) return `${base}，${missing} 项待安装`;
  if (updates) return `${base}，${updates} 项待更新`;
  return base;
}

function hasUpstreamSource(source: ExtensionSourceConfig, handlers: UnitHandlers) {
  const upstream = normalizePath(source.upstream_repository || '');
  if (!upstream) return false;
  return [...handlers.statuses.values()].some(item => item.source.kind !== 'local' && normalizePath(item.source.repository) === upstream);
}

function upstreamTitle(upstream: string, added?: boolean) {
  if (!upstream) return '未关联 GitHub 仓库，可在扩展清单或 Git 中配置';
  return added ? `已添加 GitHub 来源：${upstream}` : `关联仓库 ${upstream}，可添加为发布来源`;
}

function otherSideLabel(side: string) {
  return side === 'local' ? '本地源码' : 'GitHub 发布';
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

function MenuItem({ icon, label, title, disabled, onClick }: {
  icon: ReactNode;
  label: string;
  title?: string;
  disabled?: boolean;
  onClick: () => void;
}) {
  return <button type="button" role="menuitem" title={title} disabled={disabled} onClick={onClick}>
    {icon}
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

function SourceSwitch({ title, label, checked, disabled, onChange }: { title: string; label: string; checked: boolean; disabled: boolean; onChange: (value: boolean) => void }) {
  return <label className="toggle compact" title={title}>
    <input type="checkbox" checked={checked} disabled={disabled} aria-label={label} onChange={event => onChange(event.target.checked)} />
    <span className="slider" />
  </label>;
}

function IconAction({ icon, label, title, danger, active, disabled, onClick }: {
  icon: ReactNode;
  label: string;
  title?: string;
  danger?: boolean;
  active?: boolean;
  disabled?: boolean;
  onClick: () => void;
}) {
  return <button type="button" className={`btn btn-icon${danger ? ' is-danger' : ''}${active ? ' is-on' : ''}`} title={title || label} aria-label={label} aria-pressed={active} disabled={disabled} onClick={onClick}>{icon}</button>;
}

function VerificationSegment({ value, disabled, onChange }: {
  value: ExtensionSourceConfig['verification'];
  disabled: boolean;
  onChange: (value: ExtensionSourceConfig['verification']) => void;
}) {
  const options = [
    { key: 'required' as const, icon: <ShieldCheck size={14} />, label: '仅安装已签名版本' },
    { key: 'optional' as const, icon: <ShieldAlert size={14} />, label: '允许未签名版本' },
  ];
  return <div className="extension-source-segment" role="group" aria-label="签名校验">
    {options.map(option => <button key={option.key} type="button" className={`btn btn-icon${value === option.key ? ' is-on' : ''}`} title={option.label} aria-label={option.label} aria-pressed={value === option.key} disabled={disabled} onClick={() => onChange(option.key)}>{option.icon}</button>)}
  </div>;
}

function SourceError({ text }: { text: string }) {
  const [expanded, setExpanded] = useState(false);
  return <button type="button" className={`extension-source-error${expanded ? ' is-expanded' : ''}`} aria-expanded={expanded} title={expanded ? '点击收起' : text} onClick={() => setExpanded(value => !value)}>
    <CircleAlert size={12} />
    <span>{text}</span>
    <ChevronDown className="extension-source-error-caret" size={12} />
  </button>;
}

function SourceNotices({ notices }: { notices: ExtensionSourceNotice[] }) {
  const [expanded, setExpanded] = useState(false);
  const total = notices.reduce((sum, notice) => sum + notice.items.length, 0);
  return <div className={`extension-source-notice${expanded ? ' is-expanded' : ''}`}>
    <button type="button" className="extension-source-notice-head" aria-expanded={expanded} onClick={() => setExpanded(value => !value)}>
      <Info size={12} />
      <span>同名扩展由其他来源提供</span>
      <span className="extension-source-notice-count">{total}</span>
      <ChevronDown className="extension-source-notice-caret" size={12} />
    </button>
    {expanded ? <div className="extension-source-notice-body">
      {notices.map(notice => <div className="extension-source-notice-group" key={notice.reason}>
        <span className="extension-source-notice-reason">{notice.reason}</span>
        <div className="extension-source-notice-items">{notice.items.map(item => <span key={item}>{item}</span>)}</div>
      </div>)}
    </div> : null}
  </div>;
}

function messageOf(reason: unknown) {
  return reason instanceof Error ? reason.message : String(reason || '来源操作失败');
}

function normalizePath(value: string) {
  return value.trim().replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();
}

const MENU_WIDTH = 208;
const MENU_MAX_HEIGHT = 264;
