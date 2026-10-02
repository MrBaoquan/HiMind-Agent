import { useEffect, useMemo, useState } from 'react';
import {
  BookOpen,
  ArrowLeft,
  BadgeCheck,
  ChevronDown,
  Code2,
  CircleAlert,
  FolderOpen,
  Download,
  Link2Off,
  PlugZap,
  RotateCw,
  RefreshCw,
  Link2,
  Plus,
  Search,
  Settings2,
  Sparkles,
  Store,
  Trash2,
  Wrench,
  X,
} from 'lucide-react';
import { BusyIndicator } from '../components/BusyIndicator';
import { EmptyState, Pill, Tags } from '../components/Common';
import { ActionMenu, ActionMenuItem } from '../components/ActionMenu';
import { ExtensionKindMark } from '../components/ExtensionKindMark';
import { OperationPlanCard } from '../components/OperationPlanCard';
import type { ClientCapabilityMatrix, CodexSkillStatusItem, CodexSkillStatusResponse, ExtensionDesiredItem, ExtensionDesiredState, McpTargetDescriptor, OrganizationSkillCatalogItem, PluginCatalogItem, PluginRegistry, SkillCatalogResponse, SkillInstallPlan, SkillWorkspaceStatus } from '../services/agentApi';
import { compareSemanticVersions, installActionLabel } from './marketCatalog';

type SkillsWorkspacePageProps = {
  catalog: SkillCatalogResponse | null;
  status: CodexSkillStatusResponse | null;
  /** 客户端能力矩阵：客户端名称、支持级别与可用性的唯一来源。 */
  clientMatrix: ClientCapabilityMatrix | null;
  workspace: SkillWorkspaceStatus;
  mcpTargets: McpTargetDescriptor[];
  error: string | null;
  marketplace: OrganizationSkillCatalogItem[];
	dashboardEnabled: boolean;
	desired: ExtensionDesiredState | null;
	catalogEnabled: boolean;
	onOpenExtensions: () => void;
	availablePlugins: PluginCatalogItem[];
  busyAction: string | null;
  installTargets: InstallTarget[];
  onSyncAll: () => void;
  onClearWorkspace: () => void;
  onSyncSkill: (skillId: string) => void;
  onUpdateWorkspace: (skillId: string) => void;
  onInstallToLocation: (skillId: string) => void;
  onUpdateLocation: (skillId: string, location: string) => void;
  onRemoveLocation: (skillId: string, location: string) => void;
  onPurgeLocations: (skillId: string, locations: string[]) => void;
  onSetWorkspaceEnabled: (skillId: string, enabled: boolean) => void;
  onLoadVersions: (skillId: string) => Promise<OrganizationSkillCatalogItem[]>;
  onPlanMarketplace: (skillId: string, version?: string) => Promise<SkillInstallPlan>;
  onInstallMarketplace: (skillId: string, version: string | undefined, optionalPluginIds: string[], clients: string[], location: string) => void;
  onPickSkillLocation: () => Promise<string>;
  onRepair: (skillId: string) => void;
  onUninstall: (skillId: string) => void;
  onSyncSkillClient: (skillId: string, clientId: string) => void;
  onUnregisterClient: (skillId: string, clientId: string) => void;
  onUnregisterClients: (skillId: string) => void;
  onOpenDirectory: (path: string) => void;
  onImportLocal: () => void;
  onImportGithub: (sourceUrl: string) => Promise<void>;
};

type InstallTarget = { id: string; name: string; detected: boolean };

/**
 * 「我的能力 → 技能」看到的就是这批：自己装的技能。对接工作台后，组织管理的技能
 * 只在「组织管理」页签里出现。计数和列表必须同源，所以这段筛选对外导出给容器页用。
 */
export function installedSkills(status: CodexSkillStatusResponse | null, desired: ExtensionDesiredState | null, marketplace: OrganizationSkillCatalogItem[], dashboardEnabled: boolean) {
  const items = aggregateSkillStatusItems(status);
  return dashboardEnabled ? items.filter(item => !isManagedSkill(item, desired, marketplace)) : items;
}

export function SkillsWorkspacePage({ catalog, status, clientMatrix, workspace, mcpTargets, error, marketplace, desired, dashboardEnabled, catalogEnabled, availablePlugins, busyAction, installTargets, onSyncAll, onClearWorkspace, onSyncSkill, onInstallToLocation, onUpdateLocation, onRemoveLocation, onPurgeLocations, onSyncSkillClient, onLoadVersions, onPlanMarketplace, onInstallMarketplace, onPickSkillLocation, onRepair, onUninstall, onUnregisterClient, onUnregisterClients, onOpenDirectory, onImportLocal, onImportGithub, onOpenExtensions }: SkillsWorkspacePageProps) {
  const [query, setQuery] = useState('');
  const [selectedId, setSelectedId] = useState('');
  const [detailOpen, setDetailOpen] = useState(false);
  const [pendingUninstall, setPendingUninstall] = useState<CodexSkillStatusItem | null>(null);
  // 目录副本移除与"失效记录清理"共用一次确认：前者只动一个目录，后者一次清掉
  // 所有"目录已不存在"的幽灵记录。二者都是删除动作，都走同一条应用内确认。
  const [pendingLocationRemoval, setPendingLocationRemoval] = useState<{ skillId: string; name: string; locations: string[]; stale: boolean } | null>(null);
  const [syncAllPrompt, setSyncAllPrompt] = useState(false);
  const [installPlan, setInstallPlan] = useState<SkillInstallPlan | null>(null);
  const [planError, setPlanError] = useState('');
  const [githubOpen, setGithubOpen] = useState(false);
  const [githubSourceUrl, setGithubSourceUrl] = useState('');
  const [githubBusy, setGithubBusy] = useState(false);
  const [githubError, setGithubError] = useState('');
  const [planLoading, setPlanLoading] = useState(false);
  const items = useMemo(() => aggregateSkillStatusItems(status), [status]);
	const installedById = useMemo(() => new Map(items.map(item => [item.record.manifest.id, item])), [items]);
	const localItems = useMemo(() => installedSkills(status, desired, marketplace, dashboardEnabled), [dashboardEnabled, desired, marketplace, status]);
  useEffect(() => {
    const selectableIds = localItems.map(item => item.record.manifest.id);
    if (!selectableIds.length) {
      setSelectedId('');
    } else if (!selectableIds.includes(selectedId)) {
      setSelectedId(selectableIds[0]);
    }
  }, [localItems, selectedId]);

  const filteredItems = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    return localItems.filter(item => {
      if (!normalized) return true;
      const manifest = item.record.manifest;
      return [manifest.id, manifest.name, manifest.description, manifest.risk_summary, ...(manifest.capabilities || []).map(capability => capability.id)].join(' ').toLowerCase().includes(normalized);
    });
  }, [localItems, query]);

  const selected = filteredItems.find(item => item.record.manifest.id === selectedId) || filteredItems[0];
  const installedCount = localItems.length;
  // 与详情页"投放目标"同一口径：已探测到、或已配置为技能目标的其它 AI 工具。
  const connectedToolCount = useMemo(() => skillClientDescriptors(status, [], clientMatrix)
    .filter(client => client.id !== 'himind-ai')
    .filter(client => client.detected || Boolean(targetForSkillClient(mcpTargets, client.id)?.detected))
    .length, [status, clientMatrix, mcpTargets]);
  const isBusy = Boolean(busyAction);
  // 安装文件跟当前设置对不上（比如安装方式改过）不会自己恢复，但过去只体现在列表
  // 徽标上，用户很难注意到到底该做什么。这里汇总成页面级提示，并给一次修复全部的入口。
  const driftNotes = useMemo(() => {
    const countOf = (state: CodexSkillStatusItem['client_state']) => localItems.filter(item => item.client_state === state).length;
    const notes: string[] = [];
    const stale = countOf('render_stale');
    const modified = countOf('modified');
    const failed = countOf('failed');
    if (stale) notes.push(`${stale} 个技能的安装文件与当前设置不一致`);
    if (modified) notes.push(`${modified} 个技能的文件被改动过`);
    if (failed) notes.push(`${failed} 个技能上次同步失败`);
    return notes;
  }, [localItems]);

  async function openInstallPlan(skillId: string, version?: string) {
    setPlanLoading(true);
    setPlanError('');
    try { setInstallPlan(await onPlanMarketplace(skillId, version)); }
    catch { setPlanError('暂时无法检查技能，请稍后重试。'); }
    finally { setPlanLoading(false); }
  }

  if (!catalog && !status && !error) return <div className="page-loading"><BusyIndicator size={15} />正在读取技能</div>;

  return (
    <div className="skill-page skill-product-page">
      {error ? <div className="blocker"><CircleAlert size={18} /><div><strong>技能状态读取失败</strong><span>{error}</span></div></div> : null}

      {externalSkillTargetsUnavailable(status) ? <div className="skill-inline-warning"><CircleAlert size={15} /><span>未发现可连接的其他 AI 工具。已安装技能仍可由 HiMind AI 使用。</span></div> : null}
      {driftNotes.length ? <div className="skill-inline-warning skill-attention-note"><CircleAlert size={15} /><span>{driftNotes.join('，')}，重新同步即可恢复。</span><button type="button" className="text-action" disabled={isBusy} onClick={() => setSyncAllPrompt(true)}>立即重新同步</button></div> : null}
      {/* 页面标题归「我的能力」容器，这里只出这个类型自己的动作区。技能只有两件事：
          技能从哪来（安装）、按当前设置全量同步；状态由进入页面时的读取和统一刷新负责。 */}
      <div className="skill-toolbar-row"><div className="actions-row">
        <ActionMenu label="安装技能" icon={<Plus size={15} />} title="安装技能" panelWidth={208}>
          {close => <>
            <ActionMenuItem icon={<Store size={15} />} label="浏览技能市场" onClick={() => { onOpenExtensions(); close(); }} />
            <div className="app-menu-separator" role="separator" />
            <ActionMenuItem icon={<Link2 size={15} />} label="从 GitHub 导入" onClick={() => { setGithubError(''); setGithubOpen(true); close(); }} />
            <ActionMenuItem icon={<FolderOpen size={15} />} label="从本地导入" onClick={() => { onImportLocal(); close(); }} />
          </>}
        </ActionMenu>
        <button className="btn" title="按当前设置把全部已安装技能同步到可用 AI 工具" onClick={() => setSyncAllPrompt(true)} disabled={isBusy || !items.length}>{busyAction === 'sync-all' ? <BusyIndicator size={16} /> : <PlugZap size={16} />}{busyAction === 'sync-all' ? '同步中' : '同步全部'}</button>
      </div></div>

      <section className={`skill-workspace compact-master-detail ${detailOpen ? 'detail-open' : ''}`}>
        <aside className="skill-browser">
          {/* 数量挂在它描述的列表头上：页签已经写过「技能 N」，动作条上再飘一个计数
              就是同一屏里的第三遍，也让「安装 / 同步 / 刷新」这排动作看起来更挤。 */}
          <div className="skill-browser-header"><strong>已安装</strong><span className="section-count">{installedCount}</span></div>
          <div className="skill-browser-tools">
            <label className="skill-search"><Search size={15} /><input value={query} onChange={event => setQuery(event.target.value)} placeholder="搜索技能" /></label>
          </div>
          <div className="skill-browser-list">
			{filteredItems.map(item => <SkillListItem key={item.record.manifest.id} item={item} selected={item.record.manifest.id === selected?.record.manifest.id} onSelect={id => { setSelectedId(id); setDetailOpen(true); }} />)}
			{!filteredItems.length ? <EmptyState icon={BookOpen} title="没有匹配的技能" text="调整搜索内容或筛选条件。" /> : null}
          </div>
        </aside>

        <main className="skill-detail">
		  <button className="workspace-back" onClick={() => setDetailOpen(false)}><ArrowLeft size={15} />返回技能列表</button>
          {selected ? <SkillDetail item={selected} workspace={workspace} clientStatus={status} clientMatrix={clientMatrix} availablePlugins={availablePlugins} catalogPolicy={marketplace.find(item => item.skill_id === selected.record.manifest.id)} busyAction={busyAction} onLoadVersions={onLoadVersions} onPlanVersion={(version) => void openInstallPlan(selected.record.manifest.id, version)} onSync={onSyncSkill} onInstallToLocation={onInstallToLocation} onUpdateLocation={onUpdateLocation} onRemoveLocation={(skillId, location) => setPendingLocationRemoval({ skillId, name: selected.record.manifest.name || skillId, locations: [location], stale: false })} onPurgeLocations={(skillId, locations) => setPendingLocationRemoval({ skillId, name: selected.record.manifest.name || skillId, locations, stale: true })} onRepair={onRepair} onUninstall={() => setPendingUninstall(selected)} onSyncSkillClient={onSyncSkillClient} onUnregisterClient={onUnregisterClient} onUnregisterClients={onUnregisterClients} onOpenDirectory={onOpenDirectory} /> : <EmptyState icon={Sparkles} title="选择一个技能" text="查看功能、依赖和版本。" />}
        </main>
      </section>

      {pendingUninstall ? <div className="skill-dialog-backdrop" role="presentation"><div className="skill-dialog" role="dialog" aria-modal="true" aria-labelledby="skill-uninstall-title">
        <div className="skill-dialog-head"><strong id="skill-uninstall-title">{workspace.valid ? '从项目移除技能' : '卸载技能'}</strong><button className="btn btn-icon" aria-label="关闭" onClick={() => setPendingUninstall(null)}><X size={16} /></button></div>
        <p>{workspace.valid ? <>将从当前项目中移除 <strong>{pendingUninstall.record.manifest.name}</strong>，全局技能和其他项目不受影响。</> : <>将卸载 <strong>{pendingUninstall.record.manifest.name}</strong>，并从已连接的 AI 工具中移除。</>}</p>
        {pendingUninstall.modified_files.length ? <div className="skill-dialog-warning"><CircleAlert size={16} />检测到用户修改。请先使用“修复并备份”保留当前文件。</div> : null}
        <div className="skill-dialog-actions"><button className="btn" onClick={() => setPendingUninstall(null)}>取消</button><button className="btn btn-danger" disabled={isBusy || pendingUninstall.modified_files.length > 0} onClick={() => { onUninstall(pendingUninstall.record.manifest.id); setPendingUninstall(null); }}><Trash2 size={15} />{workspace.valid ? '从项目移除' : '确认卸载'}</button></div>
      </div></div> : null}

      {/* 从某个安装位置移除副本：只动这个目录，全局与其他目录不受影响。 */}
      {pendingLocationRemoval ? <div className="skill-dialog-backdrop" role="presentation"><div className="skill-dialog" role="dialog" aria-modal="true" aria-labelledby="skill-location-remove-title">
        <div className="skill-dialog-head"><strong id="skill-location-remove-title">{pendingLocationRemoval.stale ? '清理失效记录？' : '从该目录移除技能？'}</strong><button className="btn btn-icon" aria-label="关闭" onClick={() => setPendingLocationRemoval(null)}><X size={16} /></button></div>
        {pendingLocationRemoval.stale
          ? <p>将清掉 <strong>{pendingLocationRemoval.name}</strong> 的 <strong>{pendingLocationRemoval.locations.length}</strong> 条失效记录。这些目录已经不存在，只影响 Agent 自己的安装台账。</p>
          : <p>将移除 <strong>{pendingLocationRemoval.name}</strong> 在 <strong>{pendingLocationRemoval.locations[0]}</strong> 里的副本及安装记录；其他目录与全局技能目录不受影响。</p>}
        <div className="skill-dialog-actions"><button className="btn" onClick={() => setPendingLocationRemoval(null)}>取消</button><button className="btn btn-danger" disabled={isBusy} onClick={() => { const pending = pendingLocationRemoval; setPendingLocationRemoval(null); if (pending.stale) onPurgeLocations(pending.skillId, pending.locations); else onRemoveLocation(pending.skillId, pending.locations[0]); }}><Trash2 size={15} />{pendingLocationRemoval.stale ? '确认清理' : '确认移除'}</button></div>
      </div></div> : null}

      {/* 全量同步会一次性改写多个 AI 工具目录下的所有技能副本，先说清影响面再执行。 */}
      {syncAllPrompt ? <div className="skill-dialog-backdrop" role="presentation"><div className="skill-dialog" role="dialog" aria-modal="true" aria-labelledby="skill-sync-all-title">
        <div className="skill-dialog-head"><strong id="skill-sync-all-title">同步全部技能</strong><button className="btn btn-icon" aria-label="关闭" onClick={() => setSyncAllPrompt(false)}><X size={16} /></button></div>
        <p>将按当前设置重新生成 <strong>{localItems.length}</strong> 个已安装技能的文件，并更新到 <strong>{connectedToolCount}</strong> 个已连接的 AI 工具。</p>
        <div className="skill-dialog-actions"><button className="btn" onClick={() => setSyncAllPrompt(false)}>取消</button><button className="btn btn-primary" disabled={isBusy} onClick={() => { setSyncAllPrompt(false); onSyncAll(); }}><PlugZap size={15} />同步全部</button></div>
      </div></div> : null}

	  {installPlan || planError ? <InstallPlanDialog plan={installPlan} error={planError} currentVersion={installPlan ? installedById.get(installPlan.skill.skill_id)?.record.manifest.version : undefined} busy={isBusy} onClose={() => { setInstallPlan(null); setPlanError(''); }} clients={installTargets} onPickLocation={onPickSkillLocation} onInstall={(optionalIds, clients, location) => { if (installPlan) onInstallMarketplace(installPlan.skill.skill_id, installPlan.skill.version, optionalIds, clients, location); setInstallPlan(null); }} /> : null}
      {githubOpen ? <div className="modal-backdrop" role="presentation"><div className="modal" role="dialog" aria-modal="true" aria-labelledby="github-skill-title"><div className="modal-header"><div><h3 id="github-skill-title">从 GitHub 导入技能</h3><p>粘贴仓库链接。需要时可在链接中指定子目录和版本。</p></div><button className="btn btn-icon" aria-label="关闭" title="关闭" onClick={() => setGithubOpen(false)}><X size={16} /></button></div><div className="modal-body"><div className="field-group"><label className="field-label" htmlFor="github-skill-source-url">GitHub 链接</label><input id="github-skill-source-url" value={githubSourceUrl} onChange={event => setGithubSourceUrl(event.target.value)} placeholder="https://github.com/owner/repository.git?path=/skills/example#v1.0.0" /></div>{githubError ? <div className="inline-feedback visible" role="status">{githubError}</div> : null}<div className="modal-actions"><span /><div className="actions-row"><button className="btn" onClick={() => setGithubOpen(false)}>取消</button><button className="btn btn-primary" disabled={githubBusy || !githubSourceUrl.trim()} onClick={async () => { setGithubBusy(true); setGithubError(''); try { await onImportGithub(githubSourceUrl.trim()); setGithubOpen(false); } catch (error) { setGithubError(error instanceof Error ? error.message : 'GitHub 技能导入失败'); } finally { setGithubBusy(false); } }}>{githubBusy ? '导入中...' : '导入技能'}</button></div></div></div></div></div> : null}
    </div>
  );
}

const DISTRIBUTED_SKILL_STATES: CodexSkillStatusItem['client_state'][] = ['installed', 'outdated', 'modified', 'render_stale', 'managed_elsewhere'];

const SKILL_CLIENT_NAMES: Record<string, string> = {
  'himind-ai': 'HiMind AI',
  codex: 'Codex',
  'github-copilot': 'GitHub Copilot',
  workbuddy: 'WorkBuddy',
  claude: 'Claude',
  cursor: 'Cursor',
  windsurf: 'Windsurf',
  cline: 'Cline',
  trae: 'Trae',
  codebuddy: 'CodeBuddy',
  qoder: 'Qoder',
  zcode: 'ZCode',
  antigravity: 'Antigravity',
  'gemini-cli': 'Gemini CLI',
  opencode: 'OpenCode',
  'kimi-code': 'Kimi Code',
  kiro: 'Kiro',
  'qwen-code': 'Qwen Code',
};

export type SkillClientDescriptor = {
  id: string;
  name: string;
  detected: boolean;
  supportLevel: string;
  supportNote: string;
  /** 该客户端的运行期可用性，来自客户端能力矩阵；矩阵缺失时为空。 */
  availability?: { state: string; detail: string; scope: string };
};

/**
 * 客户端清单一律以能力矩阵为准，页面不再各自维护。
 *
 * 矩阵是后端的唯一答案来源（静态能力 + 本机可用性），但它可能比技能状态慢一拍
 * 或读取失败，所以这里保留"技能状态里出现过的客户端"作为下限：矩阵到位时补全
 * 名称、支持级别与可用性；矩阵缺失时退回原来的推断，界面不会因此空掉。
 */
export function skillClientDescriptors(status: CodexSkillStatusResponse | null, mcpTargets: McpTargetDescriptor[], matrix?: ClientCapabilityMatrix | null): SkillClientDescriptor[] {
  const skillCapable = (matrix?.clients || []).filter(client => Boolean(client.capabilities?.skills));
  const ids = new Set<string>(['himind-ai', 'codex']);
  Object.keys(status?.clients || {}).forEach(id => ids.add(id));
  mcpTargets.filter(target => target.supports_skills).forEach(target => ids.add(target.skill_client_id || target.id));
  skillCapable.forEach(client => ids.add(client.id));
  const preferred = ['himind-ai', 'github-copilot', 'workbuddy', 'qoder', 'zcode', 'codex', 'claude'];
  const matrixById = new Map(skillCapable.map(client => [client.id, client]));
  return [...ids].filter(Boolean).sort((left, right) => {
    const li = preferred.indexOf(left); const ri = preferred.indexOf(right);
    if (li !== -1 || ri !== -1) return (li === -1 ? 99 : li) - (ri === -1 ? 99 : ri);
    return (SKILL_CLIENT_NAMES[left] || left).localeCompare(SKILL_CLIENT_NAMES[right] || right);
  }).map(id => {
    const clientStatus = statusForClient(status, id);
    const matrixClient = matrixById.get(id);
    const availability = matrixClient?.availability;
    return {
      id,
      name: matrixClient?.name || clientStatus?.client_name || SKILL_CLIENT_NAMES[id] || id,
      detected: Boolean((availability?.detected ?? clientStatus?.client_detected) || clientStatus?.target_exists || (clientStatus?.target_configured && clientStatus?.target_kind !== 'workspace')),
      supportLevel: matrixClient?.support_level || clientStatus?.support_level || (['himind-ai', 'codex'].includes(id) ? 'official' : 'compatible'),
      supportNote: matrixClient?.support_note || clientStatus?.support_note || '',
      availability: availability ? { state: availability.state || '', detail: availability.detail || '', scope: availability.scope || '' } : undefined,
    };
  });
}

function SkillClientIcon({ clientId, size = 16 }: { clientId: string; size?: number }) {
  if (clientId === 'himind-ai') return <Sparkles size={size} />;
  if (clientId === 'codex' || clientId === 'claude') return <Code2 size={size} />;
  if (clientId.includes('github')) return <span className="skill-client-letter">GH</span>;
  return <Wrench size={size} />;
}



function projectName(root: string) {
  return root.replace(/[\\/]+$/, '').split(/[\\/]/).pop() || '当前项目';
}

function skillClientDistributionState(clientId: string, status?: CodexSkillStatusResponse): { label: string; detail: string; tone: 'success' | 'warn' | 'danger' | 'neutral' } {
  if (!status) return { label: '正在读取', detail: '技能分发状态', tone: 'neutral' };
  const items = status.items.filter(item => item.client_state !== 'unsupported');
  if (!items.length) return { label: '暂无适用技能', detail: '0 个技能', tone: 'neutral' };
  const distributed = items.filter(item => DISTRIBUTED_SKILL_STATES.includes(item.client_state)).length;
  const pending = items.filter(item => item.client_state === 'not_installed').length;
  const repair = items.filter(item => item.client_state === 'outdated' || item.client_state === 'modified' || item.client_state === 'render_stale').length;
  const blocked = items.filter(item => item.client_state === 'blocked' || item.client_state === 'failed').length;
  const foreign = items.filter(item => item.client_state === 'managed_elsewhere');
  const detail = `${distributed}/${items.length} 个技能`;
  if (blocked) return { label: `${blocked} 个技能不可用`, detail, tone: 'danger' };
  if (repair) return { label: `${repair} 个技能需处理`, detail, tone: 'warn' };
  if (pending) return { label: `${pending} 个技能待同步`, detail, tone: 'warn' };
  if (foreign.length === items.length) {
    return { label: '已同步', detail, tone: 'success' };
  }
  return { label: clientId === 'himind-ai' ? '技能直接可用' : '技能已同步', detail, tone: 'success' };
}

export function targetForSkillClient(targets: McpTargetDescriptor[], clientId: string) {
  return targets
    .filter(target => (target.skill_client_id || target.id) === clientId)
    .sort((left, right) => Number(right.state === 'configured') - Number(left.state === 'configured') || Number(right.detected) - Number(left.detected))[0];
}

function statusForClient(status: CodexSkillStatusResponse | null, clientId: string) {
  if (!status) return undefined;
  return status.clients?.[clientId] || (clientId === 'codex' ? status : undefined);
}

function skillClientConnectionState(clientId: string, target?: McpTargetDescriptor): { label: string; tone: 'success' | 'warn' | 'danger' | 'neutral' } {
  if (clientId === 'himind-ai') return { label: '内置可用', tone: 'success' };
  if (!target) return { label: '暂不支持', tone: 'neutral' };
  if (target.state === 'configured') return { label: '已连接', tone: 'success' };
  if (target.state === 'needs_repair') return { label: '连接需更新', tone: 'warn' };
  if (target.state === 'invalid_config') return { label: '连接异常', tone: 'danger' };
  if (target.detected) return { label: '可连接', tone: 'neutral' };
  return { label: '未连接', tone: 'neutral' };
}

function externalSkillTargetsUnavailable(status: CodexSkillStatusResponse | null) {
  if (!status) return false;
  return Object.entries(status.clients || {}).filter(([clientId]) => clientId !== 'himind-ai').every(([, client]) => !client.client_detected && !client.target_exists && client.target_mode === 'preview');
}

function aggregateSkillStatusItems(status: CodexSkillStatusResponse | null): CodexSkillStatusItem[] {
  if (!status) return [];
  const clients = status.clients ? Object.values(status.clients) : [status];
  const records = new Map<string, CodexSkillStatusItem>();
  clients.forEach(client => client.items.forEach(item => {
    if (!records.has(item.record.manifest.id)) records.set(item.record.manifest.id, item);
  }));
  return [...records.values()].map(base => {
    const variants = clients
      .map(client => client.items.find(item => item.record.manifest.id === base.record.manifest.id))
      .filter((item): item is CodexSkillStatusItem => Boolean(item));
    const supported = variants.filter(item => item.client_state !== 'unsupported');
    const state = aggregateClientState(supported.map(item => item.client_state));
    const representative = supported.find(item => item.client_state === state) || supported[0] || base;
    const availableActions = new Set(supported.flatMap(item => item.available_actions));
    if (base.record.manifest.scope === 'builtin') availableActions.delete('uninstall');
    else availableActions.add('uninstall');
    return {
      ...representative,
      client_state: state,
      rendered: supported.some(item => item.rendered),
      rendered_valid: supported.some(item => item.rendered_valid),
      installed_version: supported.find(item => item.installed_version)?.installed_version || representative.installed_version,
      modified_files: [...new Set(supported.flatMap(item => item.modified_files))],
      available_actions: [...availableActions],
    };
  });
}

function aggregateClientState(states: CodexSkillStatusItem['client_state'][]): CodexSkillStatusItem['client_state'] {
  if (!states.length) return 'unsupported';
  // 优先级说明：内容漂移（modified）最需要处理；渲染方式过期（render_stale）也要露出，
  // 但排在"有更新"之后；`installed` 高于 blocked/not_installed，因为只要有一个可用副本，
  // 这个技能就是能用的。
  for (const state of ['failed', 'modified', 'outdated', 'render_stale', 'installed', 'managed_elsewhere', 'not_installed', 'blocked'] as const) {
    if (states.includes(state)) return state;
  }
  return 'unsupported';
}

function SkillDetailTabs({ value, onChange }: { value: 'details' | 'versions'; onChange: (value: 'details' | 'versions') => void }) {
  return <div className="extension-detail-tabs" role="tablist"><button role="tab" aria-selected={value === 'details'} className={value === 'details' ? 'active' : ''} onClick={() => onChange('details')}>详情</button><button role="tab" aria-selected={value === 'versions'} className={value === 'versions' ? 'active' : ''} onClick={() => onChange('versions')}>版本</button></div>;
}

type SkillVersionDisplay = Pick<OrganizationSkillCatalogItem, 'version' | 'release_notes' | 'published_at'>;

function SkillVersionList({ versions, currentVersion, lockedLabel, loading, error, onSelect }: { versions: SkillVersionDisplay[]; currentVersion?: string; lockedLabel?: string; loading: boolean; error: string; onSelect?: (version: string) => void }) {
  const sorted = [...versions].sort((left, right) => compareSemanticVersions(right.version, left.version));
  // 本机已经是这一版的行只留状态标签：再挂一个禁用按钮只是把「已安装」写两遍。
  // 安装与升级用主按钮，降级退回普通样式；重新安装走来源管理。
  return <section className="extension-version-list">{loading ? <div className="extension-version-empty"><BusyIndicator size={15} />正在读取版本</div> : null}{error ? <div className="skill-inline-warning"><CircleAlert size={15} /><span>{error}</span></div> : null}{!loading && sorted.map(version => { const installed = currentVersion === version.version; const action = installActionLabel({ target: version.version, installed: currentVersion, locked: lockedLabel }); return <article className="extension-version-row" key={version.version}><div className="extension-version-main"><div><strong>v{version.version}</strong>{installed ? <Pill kind="success">已安装</Pill> : null}</div><time>{formatPublishedAt(version.published_at)}</time>{version.release_notes ? <p>{version.release_notes}</p> : null}</div>{onSelect && !installed ? <button className={compareSemanticVersions(version.version, currentVersion || '') > 0 ? 'btn btn-primary' : 'btn'} disabled={Boolean(lockedLabel)} onClick={() => onSelect(version.version)}>{action}</button> : null}</article>; })}</section>;
}

function InstallPlanDialog({ plan, error, currentVersion, busy, clients, onClose, onPickLocation, onInstall }: { plan: SkillInstallPlan | null; error: string; currentVersion?: string; busy: boolean; clients: { id: string; name: string; detected: boolean }[]; onClose: () => void; onPickLocation: () => Promise<string>; onInstall: (optionalIds: string[], clients: string[], location: string) => void }) {
  const [optionalIds, setOptionalIds] = useState<string[]>([]);
  // 默认投放全部工具，用户可以在安装前取消不想装的目标；"本产品自己"不参与选择。
  const [targets, setTargets] = useState<string[]>(clients.map(client => client.id));
  const [targetsTouched, setTargetsTouched] = useState(false);
  // 安装位置：默认全局，可选任意目录（只对这次安装生效）。
  const [location, setLocation] = useState("");
  useEffect(() => {
    if (targetsTouched) return;
    setTargets(clients.map(client => client.id));
  }, [clients, targetsTouched]);
  const diff = plan && currentVersion ? compareSemanticVersions(plan.skill.version, currentVersion) : 0;
  const action = !plan || !currentVersion ? '安装' : diff > 0 ? '更新' : diff === 0 ? '重新安装' : '降级';
  const targetSummary = targets.length === clients.length ? `投放到全部 ${clients.length} 个工具` : targets.length ? `投放到 ${targets.length} 个工具` : '不投放，只加入技能库';
  const locationSummary = location ? location : '全局（各 AI 工具的用户目录）';
  // 计划卡已给出每个客户端的真实落点，选择列表就用它做悬停说明，避免"投放到 12 个工具"却看不到写到哪。
  const targetPaths = new Map((plan?.plan?.targets || []).map(target => [target.id, target.destination]));
  return <div className="skill-dialog-backdrop"><div className="skill-dialog skill-plan-dialog" role="dialog" aria-modal="true">
    <div className="skill-dialog-head"><strong>{`${action}技能`}</strong><button className="btn btn-icon" onClick={onClose} aria-label="关闭"><X size={16} /></button></div>
    {error ? <div className="skill-dialog-warning"><CircleAlert size={16} />{error}</div> : null}
    {plan ? <>
      <div className="skill-plan-summary"><strong>{plan.skill.name} v{plan.skill.version}</strong><span>{plan.ready ? '可以安装' : '当前无法安装'}</span></div>
      {plan.plan ? <OperationPlanCard plan={plan.plan} heading="这次会做什么" showTargets={false} showDependencies={false} /> : null}
      {plan.plugin_actions.length ? <div className="skill-plan-actions">{plan.plugin_actions.map(action => <label key={action.plugin_id} className={action.action === 'blocked' || action.action === 'unavailable' ? 'blocked' : ''}><input type="checkbox" checked={action.required || optionalIds.includes(action.plugin_id)} disabled={action.required || !['install', 'update'].includes(action.action)} onChange={event => setOptionalIds(current => event.target.checked ? [...current, action.plugin_id] : current.filter(id => id !== action.plugin_id))} /><span><strong>{action.plugin_name || readablePluginID(action.plugin_id)}</strong><small>{installActionDescription(action.action)}</small></span><code>{action.target_version ? `v${action.target_version}` : '--'}</code></label>)}</div> : null}
      <div className="skill-plan-targets"><div className="skill-plan-targets-head"><strong>安装位置</strong><small>{locationSummary}</small></div>
        <div className="skill-plan-location"><input readOnly value={location} placeholder="默认安装到全局技能目录" /><button type="button" className="btn" disabled={busy} onClick={async () => { try { const picked = await onPickLocation(); if (picked) setLocation(picked); } catch { /* 取消选择不改动 */ } }}>选择目录…</button>{location ? <button type="button" className="text-action" disabled={busy} onClick={() => setLocation("")}>恢复全局</button> : null}</div>
      </div>
      <div className="skill-plan-targets"><div className="skill-plan-targets-head"><strong>投放目标</strong><small>{targetSummary}</small></div>
        <div className="skill-plan-target-list">{clients.map(client => <label key={client.id} title={targetPaths.get(client.id) || undefined}><input type="checkbox" checked={targets.includes(client.id)} disabled={busy} onChange={event => { setTargetsTouched(true); setTargets(current => event.target.checked ? [...current, client.id] : current.filter(id => id !== client.id)); }} /><span>{client.name}{client.detected ? '' : '（本机未检测到）'}</span></label>)}</div>
        <div className="skill-plan-targets-actions"><button type="button" className="text-action" disabled={busy} onClick={() => { setTargetsTouched(true); setTargets(clients.map(client => client.id)); }}>全选</button><button type="button" className="text-action" disabled={busy} onClick={() => { setTargetsTouched(true); setTargets([]); }}>都不投放</button></div>
      </div>
    </> : null}
    <div className="skill-dialog-actions"><button className="btn" onClick={onClose}>取消</button><button className="btn btn-primary" disabled={!plan?.ready || busy} onClick={() => onInstall(optionalIds, targets, location)}><Download size={15} />确认{action}</button></div>
  </div></div>;
}

function PluginDependencyIdentity({ pluginId, plugins }: { pluginId: string; plugins: PluginCatalogItem[] }) {
  const plugin = plugins.find(item => item.plugin_id === pluginId);
  return <span className="skill-dependency-identity"><strong>{plugin?.name || readablePluginID(pluginId)}</strong></span>;
}

function SkillListItem({ item, selected, onSelect }: { item: CodexSkillStatusItem; selected: boolean; onSelect: (id: string) => void }) {
  const manifest = item.record.manifest;
  // 安装状态与依赖可用性是两件事：插件失败时技能依然"已安装"，但列表必须能看出它现在跑不起来，
  // 否则用户要逐条点进详情才会发现问题。标签仍表达安装状态，色条表达可用性，悬停给出真实原因。
  const degraded = DISTRIBUTED_SKILL_STATES.includes(item.client_state) && item.readiness.state !== 'ready';
  const railTone = degraded ? (item.readiness.state === 'blocked' ? 'danger' : 'warn') : stateTone(item.client_state);
  const reason = item.readiness.state === 'ready' ? undefined : readinessReasonText(item);
  return <button className={`skill-browser-item ${selected ? 'selected' : ''}`} title={reason} onClick={() => onSelect(manifest.id)}><span className={`skill-state-rail ${railTone}`} /><span className="skill-browser-item-copy"><strong>{manifest.name}</strong><small>作者：{manifest.author || '未知作者'}</small><small>{manifest.description || manifest.id}</small></span><span className={`skill-state-label ${stateTone(item.client_state)}`}>{clientStateLabel(item.client_state)}</span></button>;
}

function SkillDetail({ item, workspace, clientStatus, clientMatrix, availablePlugins, catalogPolicy, busyAction, onLoadVersions, onPlanVersion, onSync, onInstallToLocation, onUpdateLocation, onRemoveLocation, onPurgeLocations, onRepair, onUninstall, onSyncSkillClient, onUnregisterClient, onUnregisterClients, onOpenDirectory }: { item: CodexSkillStatusItem; workspace: SkillWorkspaceStatus; clientStatus: CodexSkillStatusResponse | null; clientMatrix: ClientCapabilityMatrix | null; availablePlugins: PluginCatalogItem[]; catalogPolicy?: OrganizationSkillCatalogItem; busyAction: string | null; onLoadVersions: (skillId: string) => Promise<OrganizationSkillCatalogItem[]>; onPlanVersion: (version: string) => void; onSync: (id: string) => void; onInstallToLocation: (id: string) => void; onUpdateLocation: (id: string, location: string) => void; onRemoveLocation: (id: string, location: string) => void; onPurgeLocations: (skillId: string, locations: string[]) => void; onRepair: (id: string) => void; onUninstall: () => void; onSyncSkillClient: (skillId: string, clientId: string) => void; onUnregisterClient: (skillId: string, clientId: string) => void; onUnregisterClients: (skillId: string) => void; onOpenDirectory: (path: string) => void }) {
  const manifest = item.record.manifest;
  const actionBusy = Boolean(busyAction?.endsWith(manifest.id));
  const [tab, setTab] = useState<'details' | 'versions'>('details');
  const [versions, setVersions] = useState<SkillVersionDisplay[]>(catalogPolicy ? [catalogPolicy] : [{ version: manifest.version, release_notes: manifest.release_notes || '' }]);
  const [versionsLoading, setVersionsLoading] = useState(false);
  const [versionsError, setVersionsError] = useState('');
  const managed = catalogPolicy?.management !== 'user_managed' && Boolean(catalogPolicy?.managed);
  // 安装位置来自部署台账（后端 skill_locations）：技能可以同时装在全局和若干目录里，
  // 而"当前目标"的状态只反映其中一个，所以位置清单以后端聚合为准。
  const locations = useMemo(() => {
    const rows = clientStatus?.skill_locations?.[manifest.id] || [];
    const mapped = rows.map(row => {
      const sample = clientStatus?.clients ? Object.values(clientStatus.clients)
        .flatMap(client => client.items)
        .find(entry => entry.record.manifest.id === manifest.id && (row.scope === 'global' ? entry.target_scope !== 'directory' : entry.location_root === row.root)) : undefined;
      const renderedRoot = (sample?.rendered_root || '').replace(/[\\/]+$/, '');
      return {
        root: row.root,
        isGlobal: row.scope === 'global',
        clients: row.clients || [],
        version: row.version || '',
        missing: Boolean(row.missing),
        openPath: renderedRoot.slice(0, Math.max(renderedRoot.lastIndexOf('\\'), renderedRoot.lastIndexOf('/'))),
      };
    });
    return mapped.sort((left, right) => Number(right.isGlobal) - Number(left.isGlobal));
  }, [clientStatus, manifest.id]);
  // 失效记录（目录已被删掉、只剩台账）单独计数：它们不该和真实安装位置一样占一整行注意力，
  // 但也不能藏着——用户要能一眼看到并一次清干净。
  const missingLocations = useMemo(() => locations.filter(location => location.missing), [locations]);


  const workspaceManaged = workspace.managed_skills?.find(entry => entry.skill_id === manifest.id);
  const workspaceUpdateBusy = busyAction === `workspace-update:${manifest.id}`;
  const workspaceEnabledBusy = busyAction === `workspace-enabled:${manifest.id}`;

  useEffect(() => {
    if (tab !== 'versions' || !catalogPolicy) return;
    let active = true;
    setVersionsLoading(true);
    setVersionsError('');
    onLoadVersions(manifest.id).then(result => { if (active) setVersions(result); }).catch(() => { if (active) setVersionsError('暂时无法读取版本，请稍后重试。'); }).finally(() => { if (active) setVersionsLoading(false); });
    return () => { active = false; };
  }, [catalogPolicy, manifest.id, onLoadVersions, tab]);
  return <>
    <header className="skill-detail-header"><div className="skill-detail-title"><ExtensionKindMark kind="skill" /><div><div className="skill-title-line"><h3>{manifest.name}</h3><Pill kind={statePill(item.client_state)}>{clientStateLabel(item.client_state)}</Pill></div><small className="skill-detail-source">作者：{manifest.author || '未知作者'}</small></div></div><div className="skill-detail-actions">
      {/* 详情级动作只留两个：同步这一份技能、卸载。投放范围与安装目录都在对应分区里，
          不在这里重复堆入口——右上角堆成一排按钮会像补丁。 */}
      {item.available_actions.includes('repair') ? <button className="btn" disabled={actionBusy} onClick={() => onRepair(manifest.id)}>{item.client_state === 'modified' ? <><Wrench size={15} />修复并备份</> : <><PlugZap size={15} />重新同步</>}</button> : null}
      {item.available_actions.includes('uninstall') && catalogPolicy?.allow_uninstall !== false ? <button className="btn btn-danger-quiet" disabled={actionBusy} onClick={onUninstall}><Trash2 size={15} />卸载</button> : null}
    </div></header>
    <SkillDetailTabs value={tab} onChange={setTab} />
    {item.readiness.state !== 'ready' ? <div className="skill-detail-notice"><CircleAlert size={16} /><div><strong>{item.readiness.state === 'blocked' ? '当前不可安装' : '部分功能不可用'}</strong><span>{readinessReasonText(item) || '请安装所需插件或更新 HiMind Agent。'}</span></div></div> : null}
    {item.client_state === 'modified' ? <div className="skill-detail-notice modified"><Wrench size={16} /><div><strong>检测到技能文件已被修改</strong><span>修复前会自动保留一份备份。</span></div></div> : null}
    {item.client_state === 'render_stale' ? <div className="skill-detail-notice modified"><RefreshCw size={16} /><div><strong>渲染方式与当前设置不一致</strong><span>文件内容与同步记录一致，重新同步即可按当前设置重排。</span></div></div> : null}
    {tab === 'versions' ? <SkillVersionList versions={versions} currentVersion={item.installed_version || manifest.version} lockedLabel={catalogPolicy?.assignment === 'blocked' ? '不可安装' : managed ? '由组织管理' : undefined} loading={versionsLoading} error={versionsError} onSelect={catalogPolicy ? onPlanVersion : undefined} /> : <>
      {manifest.description ? <section className="skill-detail-section"><div className="skill-section-title"><div><strong>功能</strong></div></div><p className="skill-release-notes">{manifest.description}</p></section> : null}
      <section className="skill-detail-section"><div className="skill-section-title"><div><strong>投放目标</strong><small>这个技能可以被哪些 AI 工具使用</small></div></div><SkillClientAvailability status={clientStatus} clientMatrix={clientMatrix} skillId={manifest.id} busyAction={busyAction} allowUnregister={catalogPolicy?.allow_uninstall !== false} onSyncSkillClient={onSyncSkillClient} onUnregisterClient={onUnregisterClient} onUnregisterClients={onUnregisterClients} /></section>
      {/* 安装位置：技能库 → 落点目录 → 工具。与"项目/工作区"无关。 */}
      <section className="skill-detail-section">
        <div className="skill-section-title"><div><strong>安装位置</strong><small>这份技能的文件实际写到了哪些目录</small></div><span className="skill-section-actions">
          {missingLocations.length ? <button type="button" className="text-action danger" disabled={actionBusy} onClick={() => onPurgeLocations(manifest.id, missingLocations.map(location => location.root))}><Trash2 size={14} />清理 {missingLocations.length} 条失效记录</button> : null}
          <button type="button" className="text-action" disabled={actionBusy} title="把这份技能再写一份到指定目录" onClick={() => onInstallToLocation(manifest.id)}><FolderOpen size={14} />安装到指定目录…</button>
        </span></div>
        {locations.length ? <div className="skill-location-list">{locations.map(location => <div key={location.root} className={location.missing ? 'missing' : undefined}>
          <span><strong>{location.isGlobal ? '全局技能目录' : location.root}</strong><small>{location.missing ? '目录已不存在，只剩安装记录' : location.isGlobal ? '各 AI 工具的用户目录，所有项目可用' : '指定目录，只在该目录内可用'}</small></span>
          {/* 这里只回答"哪一份文件、哪个版本"。「已经投放给几个工具」由上面的
              「投放目标」回答，两个口径都写数字会出现 12 和 11 并排对不上的观感。 */}
          <small>{location.version ? `v${location.version}` : '--'}</small>
          <span className="skill-location-actions">
            {location.missing ? null : <button type="button" className="text-action" disabled={actionBusy} onClick={() => location.isGlobal ? onSync(manifest.id) : onUpdateLocation(manifest.id, location.root)}><RotateCw size={14} />更新</button>}
            {location.isGlobal ? null : <button type="button" className="text-action danger" disabled={actionBusy} onClick={() => onRemoveLocation(manifest.id, location.root)}><Trash2 size={14} />{location.missing ? '清理记录' : '移除'}</button>}
            {location.missing ? null : <button type="button" className="text-action" disabled={!location.openPath} onClick={() => location.openPath && onOpenDirectory(location.openPath)}><FolderOpen size={14} />打开</button>}
          </span>
        </div>)}</div> : <p className="skill-release-notes">还没有写入任何目录；用「重新同步」装到全局，也可装到指定目录。</p>}
      </section>
      <section className="skill-detail-section"><div className="skill-section-title"><div><strong>依赖</strong></div></div><div className="skill-dependency-list">{(manifest.plugin_dependencies || []).map(dependency => {
        // 状态点必须反映真实解析结果：之前写死绿点，用户看不出到底是哪条依赖不可用。
        const resolution = item.readiness.dependencies.find(candidate => candidate.id === dependency.plugin_id && candidate.provider === 'plugin');
        const dot = resolution?.state === 'blocked' ? 'danger' : resolution?.state === 'degraded' ? 'warn' : 'success';
        return <div key={dependency.plugin_id}>
          <span className={`status-dot ${dot}`} title={resolution?.reason || '依赖已满足'} />
          <PluginDependencyIdentity pluginId={dependency.plugin_id} plugins={availablePlugins} />
          <span>{dependency.required ? '必需' : '可选'}</span>
          <strong>{resolution?.capability_version ? `已装 v${resolution.capability_version}` : dependency.min_version ? `需要 v${dependency.min_version} 及以上` : '不限版本'}</strong>
          {resolution?.reason ? <small className="skill-dependency-reason">{resolution.reason}</small> : null}
        </div>;
      })}{!manifest.plugin_dependencies?.length ? <span className="skill-section-empty">无依赖</span> : null}</div></section>
      <details className="plugin-technical-panel"><summary>开发者信息</summary><div className="plugin-technical-grid"><div><span>技能 ID</span><code>{manifest.id}</code></div><div><span>作者</span><strong>{manifest.author || '未知作者'}</strong></div><div><span>来源</span><strong>{scopeLabel(manifest.scope)}</strong></div><div><span>最近同步</span><strong>{formatSyncedAt(item.last_synced_at)}</strong></div><div className="wide"><span>本地目录</span><code>{item.rendered_root || '--'}</code></div></div><div className="skill-file-summary"><span /><button className="text-action" disabled={!item.rendered} onClick={() => onOpenDirectory(item.rendered_root)}><FolderOpen size={14} />打开目录</button></div></details>
    </>}
  </>;
}

function SkillClientAvailability({ status, clientMatrix, skillId, busyAction, allowUnregister, onSyncSkillClient, onUnregisterClient, onUnregisterClients }: { status: CodexSkillStatusResponse | null; clientMatrix: ClientCapabilityMatrix | null; skillId: string; busyAction: string | null; allowUnregister: boolean; onSyncSkillClient: (skillId: string, clientId: string) => void; onUnregisterClient: (skillId: string, clientId: string) => void; onUnregisterClients: (skillId: string) => void }) {
  const clients = skillClientDescriptors(status, [], clientMatrix);
  const relevant = clients.filter(client => client.id === 'himind-ai' || client.detected || DISTRIBUTED_SKILL_STATES.includes(statusForClient(status, client.id)?.items.find(candidate => candidate.record.manifest.id === skillId)?.client_state || 'not_installed'));
  const external = relevant.filter(client => client.id !== 'himind-ai');
  const skillItems = new Map(external.map(client => [client.id, statusForClient(status, client.id)?.items.find(candidate => candidate.record.manifest.id === skillId)]));
  // 计数必须互斥：同一个工具不能既算「已同步」又算「需处理」，否则会出现
  // 「已同步到 10 个工具 · 10 个需处理」这种自相矛盾的摘要。
  // 口径必须覆盖展开后列出的每一行（含 HiMind AI 本身），否则摘要写 11、
  // 展开却列出 12 行，两个数字在同一屏里对不上。
  const states = relevant.map(client =>
    statusForClient(status, client.id)?.items.find(candidate => candidate.record.manifest.id === skillId)?.client_state || 'not_installed');
  const current = states.filter(state => state === 'installed' || state === 'managed_elsewhere').length;
  const outdated = states.filter(state => state === 'outdated').length;
  const repair = states.filter(state => state === 'modified' || state === 'render_stale').length;
  const pending = states.filter(state => state === 'not_installed').length;
  const blocked = states.filter(state => state === 'blocked' || state === 'failed').length;
  const renderClient = (client: (typeof clients)[number]) => {
    const item = statusForClient(status, client.id)?.items.find(candidate => candidate.record.manifest.id === skillId);
    const state = skillAvailabilityState(client.id, item);
    const canUnregister = allowUnregister && client.id !== 'himind-ai' && ['installed', 'outdated', 'render_stale'].includes(item?.client_state || '');
    const unregisterBusy = busyAction === `unregister:${client.id}:${skillId}`;
    const registerable = client.id !== 'himind-ai' && ['not_installed', 'outdated', 'modified', 'render_stale'].includes(item?.client_state || '') && client.detected;
    const registerBusy = busyAction === `register:${client.id}:${skillId}`;
    // 每行写清动作文案：只有图标时用户必须悬停才知道那个符号是"投放"还是"移除"，
    // 一屏十几个未标注图标正是"像补丁"的来源。
    return <div className="skill-client-tool" key={client.id}><span className={`skill-client-mini-icon ${client.id === 'himind-ai' ? 'himind-ai' : ''}`}><SkillClientIcon clientId={client.id} size={14} /></span><span className="skill-client-tool-copy"><strong title={client.name}>{client.name}</strong><small className={state.tone}>{state.label}</small></span>{canUnregister ? <button type="button" className="text-action danger skill-client-action" disabled={unregisterBusy} onClick={() => onUnregisterClient(skillId, client.id)}>{unregisterBusy ? <BusyIndicator size={12} /> : null}移除</button> : registerable ? <button type="button" className="text-action skill-client-action" disabled={registerBusy} onClick={() => onSyncSkillClient(skillId, client.id)}>{registerBusy ? <BusyIndicator size={12} /> : null}投放</button> : <span className="skill-client-tool-action-space" />}</div>;
  };
  if (!relevant.length) return <div className="skill-client-availability"><span className="skill-section-empty">暂无可用的 AI 工具</span></div>;
  const summaryParts: string[] = [];
  if (current) summaryParts.push(`${current} 个已投放`);
  if (outdated) summaryParts.push(`${outdated} 个待更新`);
  if (repair) summaryParts.push(`${repair} 个待重新同步`);
  if (pending) summaryParts.push(`${pending} 个未投放`);
  if (blocked) summaryParts.push(`${blocked} 个不可用`);
  const summary = status ? summaryParts.join(' · ') || '没有可同步的工具' : '正在读取同步状态';
  const canUnregisterAll = allowUnregister && external.some(client => ['installed', 'outdated', 'render_stale', 'managed_elsewhere'].includes(skillItems.get(client.id)?.client_state || ''));
  // 状态点直接反映投放健康度：有阻塞是危险色，有待处理是警示色，全绿才是成功色。
  const summaryTone = !status ? 'neutral' : blocked ? 'danger' : outdated || repair || pending ? 'warn' : 'success';
  // 摘要行只做"说明 + 一个例外动作"：全量重新同步已经在标题栏，这里不再摆第二个同步按钮。
  return <div className="skill-client-availability"><div className="skill-client-distribution-summary"><span className={`status-dot ${summaryTone}`} /><span className="skill-client-summary-text"><strong>{summary}</strong></span><div className="skill-client-summary-actions">{canUnregisterAll ? <button type="button" className="text-action danger" disabled={Boolean(busyAction)} onClick={() => onUnregisterClients(skillId)}><Link2Off size={14} />停止全部投放</button> : null}</div></div><details className="skill-client-tools"><summary><span><Settings2 size={14} /><strong>按工具设置</strong><small>{relevant.length}</small></span><ChevronDown className="skill-client-tools-chevron" size={15} /></summary><div>{relevant.map(renderClient)}</div></details></div>;
}

function skillAvailabilityState(clientId: string, item?: CodexSkillStatusItem): { label: string; tone: 'success' | 'warn' | 'danger' | 'neutral' } {
  const state = item?.client_state;
  if (!state || state === 'unsupported') return { label: '不支持', tone: 'neutral' };
  if (state === 'installed') return { label: clientId === 'himind-ai' ? '直接可用' : '已同步', tone: 'success' };
  if (state === 'managed_elsewhere') return { label: '由其他安装管理', tone: 'success' };
  if (state === 'not_installed') return { label: '未投放', tone: 'neutral' };
  if (state === 'outdated') return { label: '可更新', tone: 'warn' };
  if (state === 'modified') return { label: '已修改', tone: 'warn' };
  if (state === 'render_stale') return { label: '需重新同步', tone: 'warn' };
  if (state === 'blocked') return { label: '依赖未满足', tone: 'danger' };
  return { label: '同步失败', tone: 'danger' };
}

function isManagedPolicy(item: ExtensionDesiredItem) {
  return item.management !== 'user_managed' || item.intent === 'required' || item.desired_state === 'absent';
}

function isManagedCatalogSkill(item: OrganizationSkillCatalogItem) {
  return item.source === 'system' || item.management === 'builtin' || item.management === 'organization_managed' || ['required', 'blocked'].includes(item.assignment || '');
}

function isManagedSkill(item: CodexSkillStatusItem, desired: ExtensionDesiredState | null, marketplace: OrganizationSkillCatalogItem[]) {
  const skillId = item.record.manifest.id;
  if (item.record.manifest.scope === 'builtin') return true;
  if (desired?.items.some(policy => policy.asset_kind === 'skill' && policy.asset_key === skillId && isManagedPolicy(policy))) return true;
  return marketplace.some(policy => policy.skill_id === skillId && isManagedCatalogSkill(policy));
}

/// 把 readiness 的原因翻成一句人话：优先显示阻塞/降级依赖的具体原因。
function readinessReasonText(item: CodexSkillStatusItem) {
  const unmet = item.readiness.dependencies.filter(dependency => dependency.state !== 'ready' && dependency.required);
  if (!unmet.length) return item.readiness.reasons[0] || '';
  return unmet
    .map(dependency => {
      const detail = dependency.reason || '依赖未满足';
      return dependency.provider === 'plugin' ? `插件 ${dependency.id}：${detail}` : `能力 ${dependency.id}：${detail}`;
    })
    .join('；');
}

function clientStateLabel(state: CodexSkillStatusItem['client_state']) {
  const labels: Record<CodexSkillStatusItem['client_state'], string> = { not_installed: '未安装', installed: '已安装', outdated: '有更新', modified: '已修改', render_stale: '需重新同步', managed_elsewhere: '其他实例已同步', blocked: '不可用', unsupported: '不兼容', failed: '失败' };
  return labels[state] || state;
}

function stateTone(state: CodexSkillStatusItem['client_state']) { if (state === 'installed' || state === 'managed_elsewhere') return 'success'; if (state === 'outdated' || state === 'modified' || state === 'render_stale') return 'warn'; if (state === 'not_installed') return 'neutral'; return 'danger'; }
function statePill(state: CodexSkillStatusItem['client_state']): 'success' | 'warn' | 'danger' { if (state === 'installed' || state === 'managed_elsewhere') return 'success'; if (state === 'outdated' || state === 'modified' || state === 'render_stale' || state === 'not_installed') return 'warn'; return 'danger'; }
function scopeLabel(scope: string) { if (scope === 'builtin') return '系统内置'; if (scope === 'organization') return '技能市场'; if (scope === 'user') return '我的技能'; return scope || '--'; }
function formatSyncedAt(value?: string | null) { if (!value) return '尚未同步'; const milliseconds = Number.parseInt(value.split('-')[0], 10); return Number.isFinite(milliseconds) ? new Date(milliseconds).toLocaleString('zh-CN', { hour12: false }) : value; }
function formatPublishedAt(value?: string) { if (!value) return '发布时间未知'; const date = new Date(value); return Number.isNaN(date.getTime()) ? value : date.toLocaleDateString('zh-CN'); }
function installActionDescription(action: string) { return ({ satisfied: '已安装', install: '将一并安装', update: '将一并更新', blocked: '被组织策略阻止', unavailable: '当前不可用' } as Record<string, string>)[action] || '需要处理'; }
function readablePluginID(value: string) { const tail = value.split('.').filter(Boolean).pop() || value; return tail.split(/[-_]/).filter(Boolean).map(part => part.charAt(0).toUpperCase() + part.slice(1)).join(' '); }
