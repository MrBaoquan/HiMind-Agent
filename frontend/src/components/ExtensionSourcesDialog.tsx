import { useEffect, useMemo, useState } from 'react';
import { ChevronDown, CircleAlert, Download, FolderOpen, GitBranch, Info, MessageCircle, MoreHorizontal, Plus, RefreshCw, ShieldAlert, ShieldCheck, Trash2, X } from 'lucide-react';
import { agentApi, type DistributionStateEntry, type ExtensionDistributionUnit, type ExtensionSourceAcquisition, type ExtensionSourceConfig, type ExtensionSourceNotice, type ExtensionSourceSettings, type ExtensionSourceSnapshot, type ExtensionSourceStatus, type ExtensionWorkspaceSettings } from '../services/agentApi';
import { compareVersions, countUnitUpdates, friendlySourceName, sourceBaseName, type UnitUpdateAsset } from '../pages/marketCatalog';
import { ActionMenu, ActionMenuItem } from './ActionMenu';
import { BusyIndicator } from './BusyIndicator';

type Props = {
  open: boolean;
  /// 当前绑定的开发目录。只用来标注「这个本地仓库就是你正在开发的那个」，不再作为选择器。
  workspace: ExtensionWorkspaceSettings;
  settings: ExtensionSourceSettings;
  snapshot: ExtensionSourceSnapshot | null;
  /// 制品 ID → 市场认定的更新目标版本（`kind:id` 为键）。用的是市场清单已经
  /// 算好的那一份，让「来源卡片待更新数」和市场的「可更新」永远同源。
  unitUpdateTargets: Map<string, string>;
  loading: boolean;
  error: string;
  onClose: () => void;
  onDevelopWorkspace: (root: string) => void;
  onRefresh: () => Promise<void>;
  onAdd: (name: string, repository: string, reference: string, catalogPath: string, verification: ExtensionSourceConfig['verification']) => Promise<void>;
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
  /// 制品 ID → 市场认定的更新目标版本。只统计本单元提供的那一版。
  updateTargets: Map<string, string>;
  onSetAcquisition: (unitKey: string, acquisition: ExtensionSourceAcquisition) => Promise<void>;
  /// 单元安装会按取用侧覆盖制品，其中比本机更旧的条目要先跟用户对一次版本变化。
  onInstallUnit: (request: UnitInstallRequest) => void;
  onUpdate: (source: ExtensionSourceConfig, enabled: boolean, autoUpdate: boolean, verification: ExtensionSourceConfig['verification']) => Promise<void>;
  onRemove: (sourceId: string) => Promise<void>;
  onDevelopWorkspace: (source: ExtensionSourceConfig) => Promise<void>;
  onPrefillUpstream: (source: ExtensionSourceConfig) => void;
};

/// 一项目标侧版本比本机已装版本更旧的能力：装下去就是回退。
type UnitDowngrade = { assetKind: string; assetId: string; from: string; to: string };

/// 一次单元安装的入参：装哪一侧、装谁的版本、会不会退版本。
type UnitInstallRequest = { unitKey: string; sourceId: string; side: ExtensionSourceAcquisition; downgrades: UnitDowngrade[] };

export function ExtensionSourcesDialog({ open, workspace, settings, snapshot, unitUpdateTargets, loading, error, onClose, onDevelopWorkspace, onRefresh, onAdd, onUpdate, onRemove, onSetAcquisition, onInstallUnit }: Props) {
  // 已发布的 GitHub Release 来自分发台账：源码目录源（仓库树）与 Release 制品是两条
  // 不同的消费路径，不在这里显示的话，用户会以为"发了 Release 但来源里看不到"。
  const [published, setPublished] = useState<DistributionStateEntry[]>([]);
  const [repository, setRepository] = useState('');
  const [name, setName] = useState('');
  const [reference, setReference] = useState('main');
  const [catalogPath, setCatalogPath] = useState('.himind/catalog.json');
  const [verification, setVerification] = useState<ExtensionSourceConfig['verification']>('required');
  const [formOpen, setFormOpen] = useState(false);
  const [localError, setLocalError] = useState('');
  /// 待确认的降级安装：确认后按原样再走一次安装，取消则什么都不做。
  const [pendingInstall, setPendingInstall] = useState<UnitInstallRequest | null>(null);
  const statuses = useMemo(() => new Map((snapshot?.sources || []).map(item => [item.source.id, item])), [snapshot]);
  // Do not render the persisted sources as independent rows while the first
  // snapshot is still loading.  The snapshot is what proves that a local
  // directory and its GitHub repository belong to the same extension unit;
  // rendering settings alone briefly makes one unit look like two sources.
  const units = useMemo(() => snapshot ? buildUnitGroups(snapshot, settings) : [], [snapshot, settings]);
  const activeRoot = workspace.valid ? normalizePath(workspace.root) : '';
  const officialAdded = settings.sources.some(source => source.kind !== 'local' && normalizePath(source.repository) === 'mrbaoquan/himind-extensions');

  useEffect(() => {
    if (!open) return;
    setLocalError('');
    void onRefresh().catch(reason => setLocalError(messageOf(reason)));
    // The parent callback is intentionally read only when the dialog opens.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  useEffect(() => {
    if (!open) return;
    let disposed = false;
    void agentApi.extensionDistributionState()
      .then(items => {
        if (!disposed) setPublished(items.filter(item => item.target === 'github' && item.status === 'published'));
      })
      .catch(() => {
        // 台账读取失败不影响来源管理的主流程，静默降级为不显示发布信息。
        if (!disposed) setPublished([]);
      });
    return () => { disposed = true; };
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

  /// 添加来源只服务远端发布源：本机开发目录在「扩展开发」里添加，两个入口不重复。
  function openForm() {
    setCatalogPath('.himind/catalog.json');
    setFormOpen(true);
  }

  async function developWorkspace(source: ExtensionSourceConfig) {
    setLocalError('');
    try {
      onDevelopWorkspace(source.repository);
    } catch (reason) { setLocalError(messageOf(reason)); }
  }

  function prefillUpstreamSource(source: ExtensionSourceConfig) {
    setLocalError('');
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

  async function switchAcquisition(unitKey: string, acquisition: ExtensionSourceAcquisition) {
    setLocalError('');
    try { await onSetAcquisition(unitKey, acquisition); }
    catch (reason) { setLocalError(messageOf(reason)); }
  }

  /// 单元安装按取用侧覆盖已装制品。目标侧更旧时先把版本变化摆出来，确认后再装；
  /// 只换来源不做版本的场景照旧一步到位，不额外加一次点击。
  function installUnit(request: UnitInstallRequest) {
    setLocalError('');
    if (request.downgrades.length) {
      setPendingInstall(request);
      return;
    }
    void runInstallUnit(request.unitKey, request.sourceId);
  }

  async function runInstallUnit(unitKey: string, sourceId: string) {
    setLocalError('');
    try { await onInstallUnit(unitKey, sourceId); }
    catch (reason) { setLocalError(messageOf(reason)); }
  }

  const handlers: UnitHandlers = {
    loading,
    activeRoot,
    statuses,
    updateTargets: unitUpdateTargets,
    onSetAcquisition: switchAcquisition,
    onInstallUnit: installUnit,
    onUpdate: updateSource,
    onRemove: removeSource,
    onDevelopWorkspace: developWorkspace,
    onPrefillUpstream: prefillUpstreamSource,
  };

  return <>
  <div className="modal-backdrop extension-source-backdrop" role="presentation">
    <div className="modal extension-source-dialog" role="dialog" aria-modal="true" aria-labelledby="extension-source-title">
      <div className="modal-header extension-source-header">
        <h3 id="extension-source-title">来源管理</h3>
        <div className="actions-row">
          <ActionMenu variant="primary" label="添加来源" icon={<Plus size={15} />} disabled={loading}>
            {close => <>
              <ActionMenuItem icon={<GitBranch size={15} />} label="GitHub 发布源" disabled={loading} onClick={() => { close(); openForm(); }} />
              <div className="app-menu-separator" />
              <ActionMenuItem icon={<ShieldCheck size={15} />} label="HiMind 官方发布源" title={officialAdded ? '已添加' : 'MrBaoquan/himind-extensions'} disabled={loading || officialAdded} onClick={() => { close(); void addOfficialSource(); }} />
            </>}
          </ActionMenu>
          <button className="btn btn-icon" title="刷新来源" aria-label="刷新来源" disabled={loading} onClick={() => void onRefresh().catch(reason => setLocalError(messageOf(reason)))}>{loading ? <BusyIndicator size={15} /> : <RefreshCw size={15} />}</button>
          <button className="btn btn-icon" title="关闭" aria-label="关闭" onClick={onClose}><X size={15} /></button>
        </div>
      </div>
      <div className="modal-body extension-source-body">
        {formOpen ? <section className="extension-source-form" aria-label="添加 GitHub 发布源">
          <div className="extension-source-form-head">
            <strong>添加 GitHub 发布源</strong>
            <span>已发布的 Release 制品</span>
          </div>
          <div className="extension-source-form-grid">
            <div className="field-group extension-source-form-wide"><label className="field-label" htmlFor="extension-source-repository">仓库链接</label><input id="extension-source-repository" value={repository} onChange={event => setRepository(event.target.value)} placeholder="https://github.com/owner/repository" /></div>
            <div className="field-group"><label className="field-label" htmlFor="extension-source-name">名称（可选）</label><input id="extension-source-name" value={name} onChange={event => setName(event.target.value)} placeholder="默认使用仓库名" /></div>
            <div className="field-group"><label className="field-label" htmlFor="extension-source-reference">分支或版本</label><input id="extension-source-reference" value={reference} onChange={event => setReference(event.target.value)} placeholder="main" /></div>
            <div className="field-group"><label className="field-label" htmlFor="extension-source-catalog">清单文件</label><input id="extension-source-catalog" value={catalogPath} onChange={event => setCatalogPath(event.target.value)} placeholder=".himind/catalog.json" /></div>
            <div className="field-group"><label className="field-label" htmlFor="extension-source-verification">制品校验</label><select id="extension-source-verification" value={verification} onChange={event => setVerification(event.target.value as ExtensionSourceConfig['verification'])}><option value="required">仅已签名版本</option><option value="optional">允许未签名版本</option></select></div>
          </div>
          <div className="extension-source-form-actions"><button className="btn" onClick={() => setFormOpen(false)}>取消</button><button className="btn btn-primary" disabled={loading || !repository.trim() || !reference.trim() || !catalogPath.trim()} onClick={() => void addSource()}>保存</button></div>
        </section> : null}

        {(localError || error) ? <div className="skill-inline-warning"><CircleAlert size={15} /><span>{localError || error}</span></div> : null}

        {/* 列表是卡片而不是表格，所以这里只留分组计数；状态和操作在卡片内自带标签。 */}
        <div className="extension-source-columns" aria-hidden="true"><span>来源</span><span>{snapshot ? `${units.length} 个` : '读取中'}</span></div>

        <div className="extension-source-list">
          {units.map(group => <UnitRow key={group.key} group={group} handlers={handlers} published={published} />)}
            {!units.length ? snapshot ? <div className="extension-source-empty"><FolderOpen size={18} /><strong>还没有来源</strong><small>从右上角「添加来源」添加 GitHub 或官方发布源；本机开发目录在「扩展开发」里添加。</small></div> : <div className="extension-source-empty"><BusyIndicator size={18} /><strong>正在读取来源</strong></div> : null}
        </div>
      </div>
    </div>
  </div>
  {pendingInstall ? <div className="modal-backdrop is-confirm" role="presentation">
    <section className="modal extension-source-confirm" role="dialog" aria-modal="true" aria-labelledby="extension-source-confirm-title">
      <div className="modal-header">
        <div>
          <h3 id="extension-source-confirm-title">安装会把 {pendingInstall.downgrades.length} 项退到更低版本</h3>
          <p>继续会用{sideLabel(pendingInstall.side)}的低版本覆盖本机已装版本。</p>
        </div>
        <button type="button" className="btn btn-icon" title="关闭" aria-label="关闭" onClick={() => setPendingInstall(null)}><X size={16} /></button>
      </div>
      <div className="modal-body">
        <ul className="extension-source-confirm-list">
          {pendingInstall.downgrades.slice(0, 8).map(item => <li key={`${item.assetKind}:${item.assetId}`}><span>{item.assetId}</span><code>v{item.from} → v{item.to}</code></li>)}
        </ul>
        {pendingInstall.downgrades.length > 8 ? <p className="field-hint">另有 {pendingInstall.downgrades.length - 8} 项同样会退版本。</p> : null}
        <div className="modal-actions">
          <button type="button" className="btn" onClick={() => setPendingInstall(null)}>取消</button>
          <button type="button" className="btn btn-danger" onClick={() => { const action = pendingInstall; setPendingInstall(null); void runInstallUnit(action.unitKey, action.sourceId); }}>仍要安装</button>
        </div>
      </div>
    </section>
  </div> : null}
  </>;
}

function UnitRow({ group, handlers, published }: { group: UnitGroup; handlers: UnitHandlers; published: DistributionStateEntry[] }) {
  const { unit, local, remote } = group;
  // 只显示属于本单元的已发布条目：制品 ID 必须出现在该单元的清单里。
  const publishedForUnit = unit
    ? published.filter(entry => {
        const kindIds = entry.kind === 'plugin' ? unit.plugin_ids : entry.kind === 'skill' ? unit.skill_ids : unit.workflow_ids;
        return kindIds.includes(entry.id);
      })
    : [];
  const status = unitStatus(group, handlers.statuses);
  const install = unitInstallState(group, handlers.updateTargets);
  const label = unitTitle(group);
  const active = Boolean(local) && handlers.activeRoot !== '' && normalizePath(local!.repository) === handlers.activeRoot;
  const sourceId = unit ? (unit.acquisition === 'local' ? unit.local_source_id : unit.remote_source_id) : null;
  const summary = [status.text, unitSummary(group, install, handlers.statuses)].filter(Boolean).join(' · ');
  return <article className={`extension-source-unit${active ? ' is-active' : ''}`}>
    <div className="extension-source-unit-head">
      <div className="extension-source-item-main">
        <span className="extension-source-mark">{local ? <FolderOpen size={15} /> : <GitBranch size={15} />}</span>
        <div className="extension-source-item-text">
          <div className="extension-source-item-title">
            <strong title={label}>{label}</strong>
            {active ? <span className="extension-source-tag">当前工作区</span> : null}
          </div>
          <div className="extension-source-item-summary" title={summary}>
            <span className={`status-dot ${status.dot}`} />
            <span>{summary}</span>
          </div>
        </div>
      </div>
      <div className="extension-source-item-actions">
        {unit && local && remote ? <AcquisitionSegment value={unit.acquisition || 'local'} disabled={handlers.loading} onChange={value => void handlers.onSetAcquisition(unit.unit_key, value)} /> : null}
        {unit && !local && remote && unit.acquisition === 'local' ? <button type="button" className="btn btn-install" title="本地源码不可用，改用 GitHub 发布" disabled={handlers.loading} onClick={() => void handlers.onSetAcquisition(unit.unit_key, 'remote')}><GitBranch size={13} />改用 GitHub</button> : null}
        {unit ? <button className="btn btn-install" title={installTitle(group, install)} disabled={handlers.loading || unit.state !== 'ready' || !sourceId} onClick={() => sourceId && handlers.onInstallUnit({ unitKey: unit.unit_key, sourceId, side: unit.acquisition || 'local', downgrades: install.downgrades })}><Download size={14} />{installLabel(group, install)}</button> : null}
      </div>
    </div>
    <div className="extension-source-unit-members">
      {local ? <MemberRow source={local} kind="local" handlers={handlers} active={active} upstreamAdded={Boolean(remote) || hasUpstreamSource(local, handlers)} /> : null}
      {remote ? <MemberRow source={remote} kind="remote" handlers={handlers} active={false} /> : null}
    </div>
    {unit ? <DeveloperInfo group={group} handlers={handlers} /> : null}
    {unit?.other_side?.available && unit.other_side.newer_count > 0 ? <p className="extension-source-other-side"><Download size={12} /><span>{sideLabel(unit.other_side.side)}有 {unit.other_side.newer_count} 项新版本</span></p> : null}
    {install.mismatch ? <p className="extension-source-consistency"><CircleAlert size={12} /><span>已安装的是另一侧版本，重新安装会切换来源</span></p> : null}
    {install.foreign ? <p className="extension-source-consistency"><CircleAlert size={12} /><span>{install.foreign} 项非本来源安装，重新安装会切换来源</span></p> : null}
    {unit && install.downgrades.length ? <p className="extension-source-consistency"><CircleAlert size={12} /><span>{downgradeSummary(install.downgrades, unit.acquisition)}</span></p> : null}
    {publishedForUnit.length ? <p className="extension-source-consistency"><GitBranch size={12} /><span>已发布 Release：{publishedForUnit.map(entry => `${entry.id} v${entry.version}`).join('、')}</span></p> : null}
  </article>;
}

function MemberRow({ source, kind, handlers, active, upstreamAdded }: { source: ExtensionSourceConfig; kind: 'local' | 'remote'; handlers: UnitHandlers; active: boolean; upstreamAdded?: boolean }) {
  const status = handlers.statuses.get(source.id);
  const official = kind === 'remote' && normalizePath(source.repository) === 'mrbaoquan/himind-extensions';
  const upstream = (source.upstream_repository || '').trim();
  const label = displayName(source);
  const version = versionSummary(status);
  const revision = status?.source_commit ? status.source_commit.slice(0, 8) : '';
  return <div className="extension-source-member">
    <div className="extension-source-member-line">
      <span className="extension-source-kind">{kind === 'local' ? '本地源码' : 'GitHub 发布'}</span>
      <code title={source.repository}>{source.repository}</code>
      {kind === 'remote' ? <span className="extension-source-ref" title={`分支或版本 ${source.reference}`}>{source.reference}</span> : null}
      {version ? <span className="extension-source-chip" title="来源中的最新版本">{version}</span> : null}
      {kind === 'remote' && source.verification === 'optional' ? <span className="extension-source-chip is-warn" title="允许安装未签名版本">允许未签名</span> : null}
      {kind === 'local' && status?.source_dirty ? <span className="extension-source-chip is-warn" title={revision ? `源码 HEAD ${revision}，有未提交修改` : '本地有未提交修改'}>有未提交修改</span> : null}
      <span className="extension-source-member-actions">
        <SourceSwitch title="启用该来源" label={`启用 ${label}`} checked={source.enabled} disabled={handlers.loading} onChange={value => void handlers.onUpdate(source, value, source.auto_update, source.verification)} />
        <ActionMenu variant="icon" title={`${label} 的更多操作`} icon={<MoreHorizontal size={15} />} disabled={handlers.loading}>
          {close => kind === 'local' ? <>
            <ActionMenuItem icon={<MessageCircle size={15} />} label="在扩展开发中打开" title="用 AI 在这个目录里开发" disabled={handlers.loading} onClick={() => { close(); void handlers.onDevelopWorkspace(source); }} />
            {upstream && !upstreamAdded ? <ActionMenuItem icon={<GitBranch size={15} />} label="添加 GitHub 发布源" title={`添加关联仓库 ${upstream}`} disabled={handlers.loading} onClick={() => { close(); handlers.onPrefillUpstream(source); }} /> : null}
            <div className="app-menu-separator" />
            <ActionMenuItem danger icon={<Trash2 size={15} />} label="移除来源" title={`移除 ${label}`} disabled={handlers.loading} onClick={() => { close(); void handlers.onRemove(source.id); }} />
          </> : <>
            <ActionMenuItem icon={<RefreshCw size={15} />} label="自动更新" state={source.auto_update ? '已开启' : '已关闭'} disabled={handlers.loading || !source.enabled} onClick={() => { close(); void handlers.onUpdate(source, source.enabled, !source.auto_update, source.verification); }} />
            <ActionMenuItem icon={source.verification === 'required' ? <ShieldCheck size={15} /> : <ShieldAlert size={15} />} label="签名校验" state={source.verification === 'required' ? '仅已签名' : '允许未签名'} title={official ? '官方来源固定仅已签名' : '切换校验策略'} disabled={handlers.loading || official} onClick={() => { close(); void handlers.onUpdate(source, source.enabled, source.auto_update, source.verification === 'required' ? 'optional' : 'required'); }} />
            <div className="app-menu-separator" />
            <ActionMenuItem danger icon={<Trash2 size={15} />} label="移除来源" title={`移除 ${label}`} disabled={handlers.loading} onClick={() => { close(); void handlers.onRemove(source.id); }} />
          </>}
        </ActionMenu>
      </span>
    </div>
    {source.enabled && status?.error ? <SourceError text={status.error} /> : null}
    {source.enabled && status?.notices?.length ? <SourceNotices notices={status.notices} /> : null}
  </div>;
}

function AcquisitionSegment({ value, disabled, onChange }: { value: ExtensionSourceAcquisition; disabled: boolean; onChange: (value: ExtensionSourceAcquisition) => void }) {
  const options: { key: ExtensionSourceAcquisition; label: string; title: string }[] = [
    { key: 'local', label: '本地', title: '装本地工作区源码' },
    { key: 'remote', label: 'GitHub', title: '装 GitHub 已发布版本' },
  ];
  return <div className="extension-source-acquisition" role="group" aria-label="安装来源">
    <span className="extension-source-acquisition-label">安装自</span>
    {options.map(option => <button key={option.key} type="button" className={`btn${value === option.key ? ' is-on' : ''}`} title={option.title} aria-pressed={value === option.key} disabled={disabled} onClick={() => onChange(option.key)}>{option.label}</button>)}
  </div>;
}

/// 分发单元 ID、清单路径和本地 HEAD 只在开发排障时用得上，默认折叠，
/// 避免这些长串和卡片主信息抢注意力。
function DeveloperInfo({ group, handlers }: { group: UnitGroup; handlers: UnitHandlers }) {
  const { unit, local } = group;
  const status = local ? handlers.statuses.get(local.id) : undefined;
  const rows: { label: string; value: string }[] = [];
  if (unit?.distribution_id) rows.push({ label: '分发单元', value: unit.distribution_id });
  if (unit && (unit.channel || unit.catalog_id)) rows.push({ label: '通道', value: `${unit.channel || 'stable'} · ${unit.catalog_id || 'public'}` });
  if (local) {
    rows.push({ label: '清单文件', value: local.catalog_path || '未指定' });
    if (status?.source_commit) rows.push({ label: '本地 HEAD', value: status.source_commit.slice(0, 8) });
  }
  if (local?.upstream_repository) rows.push({ label: '关联仓库', value: local.upstream_repository });
  if (!rows.length) return null;
  return <details className="extension-source-tech">
    <summary>开发者信息</summary>
    <div className="extension-source-tech-grid">
      {rows.map(row => <div key={row.label}><span>{row.label}</span><code title={row.value}>{row.value}</code></div>)}
    </div>
  </details>;
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
  const source = group.local || group.remote;
  if (unitName) return friendlySourceName(unitName, source?.repository);
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

function unitInstallState(group: UnitGroup, updateTargets: Map<string, string>) {
  const unit = group.unit;
  if (!unit) return { installed: 0, attributed: 0, foreign: 0, updates: 0, missing: 0, mismatch: false, downgrades: [] as UnitDowngrade[] };
  const installed = new Map(unit.installed.map(item => [`${item.asset_kind}:${item.asset_id}`, item]));
  // 两个问题分开回答：本机有没有（installed/missing），以及它是不是来自本单元的取用侧
  // （attributed/foreign）。只按台账 source_id 判断会把本地文件安装、手工导入误判成未安装。
  let attributed = 0;
  let foreign = 0;
  let missing = 0;
  let mismatch = false;
  // 换来源时目标侧可能比本机旧（本地在跑未发布的 1.1.7，GitHub 上还是 1.0.9）。
  // 这种安装会覆盖成更旧的版本，必须显式说出来，不能只在按钮上写「切换来源」。
  const downgrades: UnitDowngrade[] = [];
  for (const asset of unit.assets) {
    const record = installed.get(`${asset.asset_kind}:${asset.asset_id}`);
    if (!record) {
      missing += 1;
      continue;
    }
    if (record.version && asset.version && compareVersions(record.version, asset.version) > 0) {
      downgrades.push({ assetKind: asset.asset_kind, assetId: asset.asset_id, from: record.version, to: asset.version });
    }
    if (record.side === 'local' || record.side === 'remote' || record.side === 'development') {
      attributed += 1;
      // 免安装的开发挂载就是本地工作区的当前内容，按本地侧计。
      if ((record.side === 'development' ? 'local' : record.side) !== unit.acquisition) mismatch = true;
    } else {
      foreign += 1;
    }
  }
  // 「待更新」与市场同一个口径，规则定义在 marketCatalog.countUnitUpdates 里，
  // 免得两边各写一份版本判定后再次分叉。
  const updates = countUnitUpdates(unit.assets as UnitUpdateAsset[], updateTargets);
  return { installed: unit.installed.length, attributed, foreign, updates, missing, mismatch, downgrades };
}

type UnitInstallState = { installed: number; attributed: number; foreign: number; updates: number; missing: number; mismatch: boolean; downgrades: UnitDowngrade[] };

/// 卡片摘要回答三件事：这份内容有多少、本机装了多少、还差多少。
function unitSummary(group: UnitGroup, install: UnitInstallState, statuses: Map<string, ExtensionSourceStatus>) {
  const parts = group.unit ? unitAssetCounts(group.unit) : memberAssetCounts(group, statuses);
  if (group.unit?.state === 'empty') parts.push('未提供扩展');
  if (install.updates) parts.push(`${install.updates} 项待更新`);
  // 「本机 N/总数」一个数字说清进度，不再单列「N 项待安装」。
  if (install.installed && group.unit?.assets.length) {
    parts.push(`本机 ${install.installed}/${group.unit.assets.length}`);
  } else if (install.installed) {
    parts.push(`本机 ${install.installed} 项`);
  }
  return parts.join(' · ');
}

function unitAssetCounts(unit: ExtensionDistributionUnit) {
  const parts: string[] = [];
  if (unit.state !== 'ready') return parts;
  if (unit.plugin_count) parts.push(`${unit.plugin_count} 插件`);
  if (unit.skill_count) parts.push(`${unit.skill_count} 技能`);
  if (unit.workflow_count) parts.push(`${unit.workflow_count} 工作流`);
  return parts;
}

function memberAssetCounts(group: UnitGroup, statuses: Map<string, ExtensionSourceStatus>) {
  const source = group.local || group.remote;
  const status = source ? statuses.get(source.id) : undefined;
  const parts: string[] = [];
  if (!status) return parts;
  if (status.plugin_count) parts.push(`${status.plugin_count} 插件`);
  if (status.skill_count) parts.push(`${status.skill_count} 技能`);
  if (status.workflow_count) parts.push(`${status.workflow_count} 工作流`);
  return parts;
}

function installLabel(group: UnitGroup, install: UnitInstallState) {
  // 来源不可读时不能引导用户"重新安装"，只能换来源或启用来源。
  if (group.unit && group.unit.state !== 'ready') return '来源不可用';
  const missing = install.missing;
  if (missing && install.updates) return '安装或更新';
  if (missing) return '安装';
  // 换来源会把一部分能力换成更旧的版本，按钮先说结果，点下去还要再确认一次。
  if (install.downgrades.length) return '切换来源并降级';
  if (install.updates) return '更新';
  // 装上的是另一侧或别的来源时，这一下会覆盖制品并改写来源记录，
  // 按钮直说结果，避免用户点完不知道到底换没换。
  if (install.mismatch || install.foreign) return '重装并切换来源';
  return install.installed ? '重新安装' : '安装';
}

/// 降级提示只说两件事：换到哪一侧、哪些能力会退版本。样例最多两条，其余计数。
function downgradeSummary(downgrades: UnitDowngrade[], acquisition: ExtensionSourceAcquisition) {
  const side = acquisition === 'local' ? '本地源码' : 'GitHub 发布';
  const samples = downgrades
    .slice(0, 2)
    .map(item => `${item.assetId} v${item.from} → v${item.to}`)
    .join('、');
  const rest = downgrades.length > 2 ? ` 等 ${downgrades.length} 项` : '';
  return `${side}版本更旧：安装后 ${samples}${rest}会回退到更低版本`;
}

function installTitle(group: UnitGroup, install: UnitInstallState) {
  const side = group.unit?.acquisition === 'remote' ? 'GitHub 发布' : '本地源码';
  const base = `从${side}安装插件、技能与工作流`;
  const missing = install.missing;
  const updates = install.updates;
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

function sideLabel(side: string) {
  return side === 'local' ? '本地源码' : 'GitHub 发布';
}

function versionSummary(status?: ExtensionSourceStatus) {
  const versions = [...new Set((status?.versions || []).map(item => item.version).filter(Boolean))];
  if (!versions.length) return '';
  if (versions.length === 1) return `v${versions[0]}`;
  const latest = versions.sort((left, right) => right.localeCompare(left, undefined, { numeric: true }))[0];
  return `最新 v${latest}`;
}

function baseName(value: string) {
  const trimmed = value.replace(/[\\/]+$/, '');
  return trimmed.split(/[\\/]/).pop() || trimmed;
}

function displayName(source: ExtensionSourceConfig) {
  const name = (source.name || '').trim();
  if (name && normalizePath(name) !== normalizePath(source.repository)) return friendlySourceName(name, source.repository);
  return sourceBaseName(source.repository);
}

function SourceSwitch({ title, label, checked, disabled, onChange }: { title: string; label: string; checked: boolean; disabled: boolean; onChange: (value: boolean) => void }) {
  return <label className="toggle compact" title={title}>
    <input type="checkbox" checked={checked} disabled={disabled} aria-label={label} onChange={event => onChange(event.target.checked)} />
    <span className="slider" />
  </label>;
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
      <span>其他来源有同名扩展</span>
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
