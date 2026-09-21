import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  ArrowLeft,
  Blocks,
  BookOpen,
  CheckCircle2,
  CircleAlert,
  Download,
  GitBranch,
  RefreshCw,
  Search,
  ShieldCheck,
  Store,
  Workflow,
  X,
} from 'lucide-react';
import { EmptyState, PageHeader, Pill, Tags } from '../components/Common';
import { ExtensionSourcesDialog } from '../components/ExtensionSourcesDialog';
import {
  FUNCTIONAL_CATEGORIES,
  categorySearchText,
  functionalCategoryLabels,
  functionalCategoryMatches,
  resolveFunctionalCategory,
} from '../data/categoryCatalog';
import {
  compareSemanticVersions,
  entryIdentity,
  marketVersionFromEntry,
  mergeMarketEntries,
  newerVersion,
  resolveSource,
  sourceDisplayLabel,
  versionIdentity,
  type MarketCandidate,
  type MarketProduct,
  type MarketSourceGroup,
} from './marketCatalog';
import type {
  CodexSkillStatusItem,
  ExtensionDistributionUnit,
  ExtensionSourceAcquisition,
  ExtensionSourceConfig,
  ExtensionSourceSettings,
  ExtensionSourceSnapshot,
  ExtensionWorkspaceSettings,
  OrganizationSkillCatalogItem,
  PluginCatalogItem,
  PluginInstallPlan,
  PluginItem,
  SkillInstallPlan,
  WorkflowCatalogItem,
  WorkflowCenterItem,
} from '../services/agentApi';

export type ExtensionKind = 'plugin' | 'skill' | 'workflow';

type MarketStateFilter = 'all' | 'available' | 'installed' | 'update';

/// One discoverable extension. Plugin, Skill and Workflow catalogs stay
/// separate on the wire; this is the read model the page renders so the three
/// kinds can be searched and compared side by side.
type MarketEntry = MarketProduct & {
  kind: ExtensionKind;
  name: string;
  description: string;
  author: string;
  risk: string;
  plugin?: PluginCatalogItem;
  skill?: OrganizationSkillCatalogItem;
  workflow?: WorkflowCatalogItem;
};

type MarketVersion = MarketCandidate;

const kindLabels: Record<ExtensionKind, string> = { plugin: '插件', skill: '技能', workflow: '工作流' };
const kindOrder: ExtensionKind[] = ['plugin', 'skill', 'workflow'];

/// 首次进入「市场」时三类目录可能仍在构建扩展源快照，按这个节奏重试。
const CATALOG_RETRY_DELAYS_MS = [0, 4000, 10000, 20000, 45000, 90000];

const sourceFilters: { id: 'all' | MarketSourceGroup; label: string }[] = [
  { id: 'all', label: '全部来源' },
  { id: 'organization', label: '组织发布' },
  { id: 'system', label: '系统内置' },
  { id: 'local', label: '本地源码' },
  { id: 'remote', label: 'GitHub 发布' },
];

const stateFilters: { id: MarketStateFilter; label: string }[] = [
  { id: 'all', label: '全部状态' },
  { id: 'available', label: '未安装' },
  { id: 'installed', label: '已安装' },
  { id: 'update', label: '可更新' },
];

function readableID(value: string) {
  const tail = value.split(/[.:]/).filter(Boolean).pop() || value;
  return tail.split(/[-_]/).filter(Boolean).map(part => part.charAt(0).toUpperCase() + part.slice(1)).join(' ');
}

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

function assignmentLabel(assignment: string | undefined, governance: string | undefined, isSystem: boolean) {
  if (governance === 'blocked' || assignment === 'blocked') return '组织已禁止';
  if (isSystem) return '系统内置';
  if (assignment === 'required') return '组织必装';
  if (assignment === 'recommended') return '组织推荐';
  return '可选安装';
}

function assignmentKind(label: string): MarketEntry['policyKind'] {
  if (label === '组织已禁止') return 'danger';
  if (label === '组织必装' || label === '组织推荐') return 'warn';
  return 'neutral';
}

type BuildInput = {
  plugins: PluginCatalogItem[];
  skills: OrganizationSkillCatalogItem[];
  workflows: WorkflowCatalogItem[];
  installedPlugins: PluginItem[];
  installedSkills: CodexSkillStatusItem[];
  installedWorkflows: WorkflowCenterItem[];
};

function buildEntries({ plugins, skills, workflows, installedPlugins, installedSkills, installedWorkflows }: BuildInput): MarketEntry[] {
  const installedPluginById = new Map(installedPlugins.map(item => [item.id, item]));
  const installedSkillById = new Map(installedSkills.map(item => [item.record.manifest.id, item]));
  const installedWorkflowById = new Map(installedWorkflows.map(item => [item.package.id, item]));
  const pluginNameById = new Map(plugins.map(item => [item.plugin_id, item.name || item.plugin_id]));

  const pluginEntries = plugins.map<MarketEntry>(item => {
    const installed = installedPluginById.get(item.plugin_id);
    const source = resolveSource(item.source);
    const policy = assignmentLabel(item.assignment, item.governance, item.source === 'system' || item.governance === 'required');
    const permissions = friendlyPermissions(item.permissions || []);
    return {
      key: entryIdentity('plugin', item.plugin_id, item.source || '', item.artifact_id || '', item.sha256 || ''),
      kind: 'plugin',
      id: item.plugin_id,
      name: item.name || item.plugin_id,
      description: item.description || '',
      author: item.author_name || (source.group === 'local' ? '本地项目' : '发布者未注明'),
      version: item.version,
      categories: item.categories || [],
      capabilityIds: item.capability_ids || [],
      dependencies: (item.plugin_dependencies || []).map(dependency => ({
        id: dependency.plugin_id,
        name: pluginNameById.get(dependency.plugin_id) || readableID(dependency.plugin_id),
        required: dependency.required,
        hint: dependency.min_version ? `v${dependency.min_version} 及以上` : '不限版本',
      })),
      sourceGroup: source.group,
      sourceLabel: source.label,
      policyLabel: policy,
      policyKind: assignmentKind(policy),
      blocked: item.governance === 'blocked' || item.assignment === 'blocked',
      // Mirrors the plugin management page: organization-managed plugins are
      // installed and versioned by policy, so no manual install is offered.
      managed: Boolean(item.managed) || (item.assignment === 'required' && item.management !== 'user_managed'),
      installedVersion: installed?.version || '',
      updateVersion: installed?.version ? newerVersion(item.version, installed.version) : '',
      risk: permissions.length ? permissions.join('、') : '未声明额外权限',
      support: item.view_count ? ['桌面工具', 'AI 工具'] : item.capability_ids?.length ? ['AI 工具'] : [],
      minAgentVersion: item.min_agent_version || '',
      source: item.source || '',
      artifactId: item.artifact_id || '',
      sha256: item.sha256 || '',
      plugin: item,
    };
  });

  const skillEntries = skills.map<MarketEntry>(item => {
    const installed = installedSkillById.get(item.skill_id);
    const source = resolveSource(item.source, item.channel);
    const policy = assignmentLabel(item.assignment, undefined, item.source === 'system');
    const installedVersion = installed && installed.client_state !== 'not_installed' ? installed.installed_version || installed.record.manifest.version : '';
    const availableVersion = installed?.available_version || item.version;
    return {
      key: entryIdentity('skill', item.skill_id, item.source || '', item.artifact_id || '', item.sha256 || ''),
      kind: 'skill',
      id: item.skill_id,
      name: item.name || item.skill_id,
      description: item.description || '',
      author: item.author_name || (source.group === 'local' ? '本地项目' : '发布者未注明'),
      version: item.version,
      categories: item.categories || [],
      capabilityIds: item.capability_ids || [],
      dependencies: (item.plugin_dependencies || []).map(dependency => ({
        id: dependency.plugin_id,
        name: pluginNameById.get(dependency.plugin_id) || readableID(dependency.plugin_id),
        required: dependency.required,
        hint: dependency.min_version ? `v${dependency.min_version} 及以上` : '不限版本',
      })),
      sourceGroup: source.group,
      sourceLabel: source.label,
      policyLabel: policy,
      policyKind: assignmentKind(policy),
      blocked: item.assignment === 'blocked',
      managed: Boolean(item.managed),
      installedVersion,
      updateVersion: installedVersion ? newerVersion(availableVersion, installedVersion) : '',
      risk: item.risk_summary || '未声明风险摘要',
      support: item.supported_clients?.length ? item.supported_clients : [],
      minAgentVersion: item.min_agent_version || '',
      source: item.source || '',
      artifactId: item.artifact_id || '',
      sha256: item.sha256 || '',
      skill: item,
    };
  });

  const workflowEntries = workflows.map<MarketEntry>(item => {
    const installed = installedWorkflowById.get(item.workflow_id);
    const source = resolveSource(item.source, item.channel);
    const policy = assignmentLabel(item.assignment, undefined, item.source === 'system');
    return {
      key: entryIdentity('workflow', item.workflow_id, item.source || '', item.artifact_id || '', item.sha256 || ''),
      kind: 'workflow',
      id: item.workflow_id,
      name: item.name || item.workflow_id,
      description: item.description || '',
      author: item.author_name || (source.group === 'local' ? '本地项目' : '发布者未注明'),
      version: item.version,
      categories: item.categories || [],
      capabilityIds: item.capability_ids || [],
      dependencies: [],
      sourceGroup: source.group,
      sourceLabel: source.label,
      policyLabel: policy,
      policyKind: assignmentKind(policy),
      blocked: item.assignment === 'blocked',
      managed: Boolean(item.managed),
      installedVersion: installed?.package.version || '',
      updateVersion: installed ? newerVersion(item.version, installed.package.version) : '',
      risk: source.group === 'local' ? '实时目录' : item.signature ? '签名已验证' : '未提供签名',
      support: source.group === 'local'
        ? ['本机调试']
        : source.group === 'remote'
          ? ['GitHub 发布']
          : item.channel
            ? [item.channel === 'stable' ? '正式版' : item.channel]
            : [],
      minAgentVersion: item.min_agent_version || '',
      source: item.source || '',
      artifactId: item.artifact_id || '',
      sha256: item.sha256 || '',
      workflow: item,
    };
  });

  return mergeMarketEntries([...pluginEntries, ...skillEntries, ...workflowEntries]);
}

function entrySearchText(entry: MarketEntry) {
  return [
    entry.name,
    entry.id,
    entry.description,
    entry.author,
    entry.sourceLabel,
    entry.capabilityIds.join(' '),
    categorySearchText(entry.categories),
    entry.dependencies.map(dependency => `${dependency.name} ${dependency.id}`).join(' '),
  ].join(' ').toLowerCase();
}

/// Workflow packages currently ship no functional categories, so an entry is
/// "uncategorized" until at least one of its raw categories maps to the shared
/// functional taxonomy.
function isUncategorized(entry: MarketEntry) {
  return !entry.categories.some(category => Boolean(resolveFunctionalCategory(category)));
}

/// Installed counts plus update availability for one distribution unit.
function unitInstallState(unit: ExtensionDistributionUnit) {
  const installed = new Map(unit.installed.map(item => [`${item.asset_kind}:${item.asset_id}`, item]));
  let updates = 0;
  let missing = 0;
  for (const asset of unit.assets) {
    const record = installed.get(`${asset.asset_kind}:${asset.asset_id}`);
    const availableDigest = asset.sha256.trim().toLowerCase();
    const installedDigest = record?.sha256?.trim().toLowerCase() || '';
    const digestChanged = Boolean(availableDigest && installedDigest && availableDigest !== installedDigest);
    // 直接挂载的开发项目在台账中使用 `development` 作为 source_id，
    // 但它属于本地取用侧，不应在市场里被误报为跨来源更新。
    const installedSide = record?.side === 'development' ? 'local' : record?.side;
    const sourceChanged = Boolean(record && (
      installedSide !== unit.acquisition
      || (installedSide !== 'local' && installedSide !== 'remote' && record.source_id !== asset.source_id)
    ));
    if (!record) missing += 1;
    if (!record || record.version !== asset.version || digestChanged || sourceChanged) updates += 1;
  }
  return { installed: unit.installed.length, updates, missing };
}


export function ExtensionsPage({
  loading,
  error,
  plugins,
  installedPlugins,
  skills,
  installedSkills,
  workflows,
  installedWorkflows,
  units,
  busyUnit,
  onRefresh,
  onInstallUnit,
  onOpenKind,
  onPlanPlugin,
  onInstallPlugin,
  onPlanSkill,
  onInstallSkill,
  onLoadPluginVersions,
  onLoadSkillVersions,
  onLoadWorkflowVersions,
  onInstallWorkflow,
  workspace,
  extensionSources,
  extensionSourceSnapshot,
  extensionSourcesLoading,
  extensionSourcesError,
  onRefreshSources,
  onAddSource,
  onAddLocalSource,
  onUpdateSourceConfig,
  onRemoveSource,
  onSetUnitAcquisition,
  onSetWorkspace,
  onDevelopWorkspace,
}: {
  loading: boolean;
  error: string;
  plugins: PluginCatalogItem[];
  installedPlugins: PluginItem[];
  skills: OrganizationSkillCatalogItem[];
  installedSkills: CodexSkillStatusItem[];
  workflows: WorkflowCatalogItem[];
  installedWorkflows: WorkflowCenterItem[];
  units: ExtensionDistributionUnit[];
  busyUnit: string;
  onRefresh: () => void;
  onInstallUnit: (unitKey: string, sourceId: string) => Promise<void>;
  onOpenKind: (kind: ExtensionKind) => void;
  onPlanPlugin: (pluginId: string, version?: string, source?: string, artifactId?: string, sha256?: string) => Promise<PluginInstallPlan>;
  onInstallPlugin: (pluginId: string, version?: string, source?: string, artifactId?: string, sha256?: string) => Promise<void>;
  onPlanSkill: (skillId: string, version?: string, source?: string, artifactId?: string, sha256?: string) => Promise<SkillInstallPlan>;
  onInstallSkill: (skillId: string, version: string | undefined, optionalPluginIds: string[], source?: string, artifactId?: string, sha256?: string) => Promise<void>;
  onLoadPluginVersions: (pluginId: string, source?: string) => Promise<PluginCatalogItem[]>;
  onLoadSkillVersions: (skillId: string, source?: string) => Promise<OrganizationSkillCatalogItem[]>;
  onLoadWorkflowVersions: (workflowId: string, source?: string) => Promise<WorkflowCatalogItem[]>;
  onInstallWorkflow: (workflowId: string, version: string, source: string, artifactId?: string, sha256?: string) => Promise<void>;
  workspace: ExtensionWorkspaceSettings;
  extensionSources: ExtensionSourceSettings;
  extensionSourceSnapshot: ExtensionSourceSnapshot | null;
  extensionSourcesLoading: boolean;
  extensionSourcesError: string;
  onRefreshSources: () => Promise<void>;
  onAddSource: (name: string, repository: string, reference: string, catalogPath: string, verification: ExtensionSourceConfig['verification']) => Promise<void>;
  onAddLocalSource: (name: string, root: string, catalogPath?: string) => Promise<void>;
  onUpdateSourceConfig: (source: ExtensionSourceConfig, enabled: boolean, autoUpdate: boolean, verification: ExtensionSourceConfig['verification']) => Promise<void>;
  onRemoveSource: (sourceId: string) => Promise<void>;
  onSetUnitAcquisition: (unitKey: string, acquisition: ExtensionSourceAcquisition) => Promise<void>;
  onSetWorkspace: (root: string) => Promise<void>;
  onDevelopWorkspace: (root: string) => void;
}) {
  const [kindFilter, setKindFilter] = useState<'all' | ExtensionKind>('all');
  const [sourceFilter, setSourceFilter] = useState<'all' | MarketSourceGroup>('all');
  const [stateFilter, setStateFilter] = useState<MarketStateFilter>('all');
  const [categoryFilter, setCategoryFilter] = useState('all');
  const [query, setQuery] = useState('');
  const [selectedKey, setSelectedKey] = useState('');
  const [detailOpen, setDetailOpen] = useState(false);
  const [pluginPlan, setPluginPlan] = useState<PluginInstallPlan | null>(null);
  const [skillPlan, setSkillPlan] = useState<SkillInstallPlan | null>(null);
  const [planError, setPlanError] = useState('');
  const [planBusy, setPlanBusy] = useState(false);
  const [installing, setInstalling] = useState('');
  const [sourcesOpen, setSourcesOpen] = useState(false);
  const catalogAttempts = useRef(0);
  // `onRefresh` is a fresh closure on every parent render; keeping it in a ref
  // stops the retry effect from resetting its timer before it can fire.
  const refreshCatalogs = useRef(onRefresh);
  refreshCatalogs.current = onRefresh;

  // The three catalogs load once at app start; the first call can still be
  // building the extension-source snapshot, which leaves the available list
  // looking empty. Retry on a patient schedule until every kind has content.
  useEffect(() => {
    if (loading) return;
    if (plugins.length && skills.length && workflows.length) return;
    const attempt = catalogAttempts.current;
    if (attempt >= CATALOG_RETRY_DELAYS_MS.length) return;
    const timer = window.setTimeout(() => {
      catalogAttempts.current += 1;
      refreshCatalogs.current();
    }, CATALOG_RETRY_DELAYS_MS[attempt]);
    return () => window.clearTimeout(timer);
  }, [loading, plugins.length, skills.length, workflows.length]);

  const entries = useMemo(
    () => buildEntries({ plugins, skills, workflows, installedPlugins, installedSkills, installedWorkflows }),
    [installedPlugins, installedSkills, installedWorkflows, plugins, skills, workflows],
  );

  const categoryCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const entry of entries) {
      for (const category of FUNCTIONAL_CATEGORIES) {
        if (functionalCategoryMatches(entry.categories, category.id)) counts.set(category.id, (counts.get(category.id) || 0) + 1);
      }
    }
    return counts;
  }, [entries]);

  const uncategorizedCount = useMemo(() => entries.filter(isUncategorized).length, [entries]);

  const visibleEntries = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    return entries.filter(entry => {
      if (kindFilter !== 'all' && entry.kind !== kindFilter) return false;
      if (sourceFilter !== 'all' && !(entry.sourceGroups || [entry.sourceGroup]).includes(sourceFilter)) return false;
      if (categoryFilter === 'uncategorized') { if (!isUncategorized(entry)) return false; }
      else if (categoryFilter !== 'all' && !functionalCategoryMatches(entry.categories, categoryFilter)) return false;
      if (stateFilter === 'available' && entry.installedVersion) return false;
      if (stateFilter === 'installed' && !entry.installedVersion) return false;
      if (stateFilter === 'update' && !entry.updateVersion) return false;
      if (normalized && !entrySearchText(entry).includes(normalized)) return false;
      return true;
    });
  }, [categoryFilter, entries, kindFilter, query, sourceFilter, stateFilter]);
  const kindCounts = useMemo(() => ({
    plugin: entries.filter(entry => entry.kind === 'plugin').length,
    skill: entries.filter(entry => entry.kind === 'skill').length,
    workflow: entries.filter(entry => entry.kind === 'workflow').length,
  }), [entries]);

  const selectedEntry = visibleEntries.find(entry => entry.key === selectedKey) || visibleEntries[0] || null;
  const availableCount = entries.filter(entry => !entry.installedVersion).length;
  const updateCount = entries.filter(entry => entry.updateVersion).length;
  const selectedBusyKey = selectedEntry?.key || '';

  const loadVersions = useCallback(async (entry: MarketEntry): Promise<MarketVersion[]> => {
    const sources = [...new Set((entry.sourceCandidates || [marketVersionFromEntry(entry)]).map(candidate => candidate.source))];
    const results = await Promise.allSettled(sources.map(async source => {
      const sourceArg = source || undefined;
      if (entry.kind === 'plugin') return onLoadPluginVersions(entry.id, sourceArg);
      if (entry.kind === 'skill') return onLoadSkillVersions(entry.id, sourceArg);
      return onLoadWorkflowVersions(entry.id, sourceArg);
    }));
    const loaded = results.flatMap(result => result.status === 'fulfilled' ? result.value.map(item => {
      const source = item.source || entry.source;
      return { version: item.version, publishedAt: item.published_at || '', notes: item.release_notes || '', source, sourceLabel: sourceDisplayLabel(source, entry.sourceLabel), minAgentVersion: item.min_agent_version || '', artifactId: item.artifact_id || '', sha256: item.sha256 || '' };
    }) : []);
    return [...new Map([...loaded, ...(entry.sourceCandidates || [marketVersionFromEntry(entry)])].map(candidate => [versionIdentity(candidate), candidate] as const)).values()];
  }, [onLoadPluginVersions, onLoadSkillVersions, onLoadWorkflowVersions]);

  async function openPluginPlan(entry: MarketEntry, candidate: MarketVersion) {
    setPlanError('');
    setPlanBusy(true);
    try {
      // 依赖检查必须绑定用户实际选中的候选来源，否则多来源产品的本地版本会被
      // 组织或 GitHub 的依赖解析结果覆盖。
      setPluginPlan(await onPlanPlugin(entry.id, candidate.version, candidate.source, candidate.artifactId, candidate.sha256));
    } catch {
      setPlanError(`暂时无法检查“${entry.name}”的依赖，请稍后重试。`);
    } finally {
      setPlanBusy(false);
    }
  }

  async function openSkillPlan(entry: MarketEntry, candidate: MarketVersion) {
    setPlanError('');
    setPlanBusy(true);
    try {
      setSkillPlan(await onPlanSkill(entry.id, candidate.version, candidate.source, candidate.artifactId, candidate.sha256));
    } catch {
      setPlanError(`暂时无法检查“${entry.name}”的依赖，请稍后重试。`);
    } finally {
      setPlanBusy(false);
    }
  }

  async function runInstall(key: string, task: () => Promise<void>) {
    setInstalling(key);
    try {
      await task();
    } finally {
      setInstalling('');
    }
  }

  function installEntry(entry: MarketEntry, candidate: MarketVersion) {
    if (entry.kind === 'plugin') { void openPluginPlan(entry, candidate); return; }
    if (entry.kind === 'skill') { void openSkillPlan(entry, candidate); return; }
    void runInstall(entry.key, () => onInstallWorkflow(entry.id, candidate.version, candidate.source, candidate.artifactId, candidate.sha256));
  }

  return (
    <div className="plugin-page market-page">
      <PageHeader
        title="市场"
        description="查找、安装和更新插件、技能与工作流。"
        actions={<div className="actions-row"><button className="btn" title="管理来源" onClick={() => setSourcesOpen(true)}><GitBranch size={14} />来源管理</button><button className="btn btn-icon" title="刷新市场" aria-label="刷新市场" onClick={onRefresh}><RefreshCw size={16} className={loading ? 'spin' : ''} /></button></div>}
      />
      {error ? <div className="blocker"><CircleAlert size={18} /><div><strong>部分目录数据读取失败</strong><span>{error}</span></div></div> : null}
      <section className="extension-summary" aria-label="市场概览">
        <div><Blocks size={18} /><span><small>插件</small><strong>{kindCounts.plugin}</strong></span></div>
        <div><BookOpen size={18} /><span><small>技能</small><strong>{kindCounts.skill}</strong></span></div>
        <div><Workflow size={18} /><span><small>工作流</small><strong>{kindCounts.workflow}</strong></span></div>
        <div className={updateCount ? 'attention' : ''}><Download size={18} /><span><small>可更新</small><strong>{updateCount}</strong></span></div>
      </section>
          {units.length ? (
            <section className="card extension-unit-panel">
              <div className="card-header"><strong>扩展仓库</strong><Pill kind="neutral">{units.length}</Pill></div>
              <div className="extension-unit-list">
                {units.map(unit => {
                  const state = unitInstallState(unit);
                  // Bind the install action to the source selected in the
                  // snapshot. A DistributionUnit can expose both local and
                  // remote sources, but installation must never silently
                  // fall back to the other side when the selected source is
                  // unavailable.
                  const sourceId = unit.acquisition === 'local' ? unit.local_source_id : unit.remote_source_id;
                   const sourceLabel = unit.acquisition === 'remote' ? 'GitHub 发布' : '本地源码';
                   const stateLabel = state.missing && state.updates > state.missing
                     ? `${state.missing} 项待安装，${state.updates - state.missing} 项可更新`
                     : state.missing
                     ? `${state.missing} 项待安装`
                     : state.updates
                       ? `${state.updates} 项可更新`
                       : state.installed
                         ? '已是最新'
                         : '未安装';
                  const parts = [
                    unit.plugin_count ? `${unit.plugin_count} 插件` : '',
                    unit.skill_count ? `${unit.skill_count} 技能` : '',
                    unit.workflow_count ? `${unit.workflow_count} 工作流` : '',
                  ].filter(Boolean);
                  return (
                    <article className="extension-unit-row" key={unit.unit_key}>
                      <span>
                        <strong>{unit.name || unit.repository}</strong>
                       <small>{sourceLabel}{sourceId ? '' : '（来源不可用）'} · {parts.length ? parts.join(' · ') : '未提供扩展'}{state.installed ? ` · 已安装 ${state.installed} 项` : ''}</small>
                      </span>
                      <Pill kind={state.updates ? 'warn' : 'neutral'}>{stateLabel}</Pill>
                      <button type="button" className="btn" disabled={Boolean(busyUnit) || unit.state !== 'ready' || !sourceId} onClick={() => sourceId && void onInstallUnit(unit.unit_key, sourceId)}>
                        <Download size={14} />{busyUnit === unit.unit_key ? '处理中' : state.missing && state.updates > state.missing ? '安装或更新' : state.missing ? '安装' : state.updates ? '更新' : state.installed ? '重新安装' : '安装'}
                      </button>
                    </article>
                  );
                })}
              </div>
            </section>
          ) : null}
        <section className={`market-workspace compact-master-detail${detailOpen ? ' detail-open' : ''}`}>
          <aside className="market-browser">
            <div className="market-tools">
              <label className="plugin-search"><Search size={15} /><span className="sr-only">搜索扩展</span><input value={query} onChange={event => setQuery(event.target.value)} placeholder="搜索名称、用途或能力" /></label>
              <div className="plugin-tabs market-kind-tabs" role="tablist" aria-label="扩展类型">
                <button role="tab" aria-selected={kindFilter === 'all'} className={kindFilter === 'all' ? 'active' : ''} onClick={() => setKindFilter('all')}>全部 <span>{entries.length}</span></button>
                {kindOrder.map(kind => <button role="tab" key={kind} aria-selected={kindFilter === kind} className={kindFilter === kind ? 'active' : ''} onClick={() => setKindFilter(kind)}>{kindLabels[kind]} <span>{kindCounts[kind]}</span></button>)}
              </div>
              <div className="market-category-block">
                <div className="market-category-heading"><strong>功能分类</strong><span>按用途查找</span></div>
                <nav className="market-category-nav" aria-label="扩展功能分类">
                  <button type="button" className={categoryFilter === 'all' ? 'active' : ''} onClick={() => setCategoryFilter('all')}>全部<span>{entries.length}</span></button>
                  {FUNCTIONAL_CATEGORIES.map(category => <button type="button" key={category.id} className={categoryFilter === category.id ? 'active' : ''} onClick={() => setCategoryFilter(category.id)}>{category.label}<span>{categoryCounts.get(category.id) || 0}</span></button>)}
                  <button type="button" className={categoryFilter === 'uncategorized' ? 'active' : ''} onClick={() => setCategoryFilter('uncategorized')}>未分类<span>{uncategorizedCount}</span></button>
                </nav>
              </div>
              <div className="market-refine">
                <label><span className="sr-only">扩展来源</span><select value={sourceFilter} onChange={event => setSourceFilter(event.target.value as 'all' | MarketSourceGroup)}>{sourceFilters.map(item => <option key={item.id} value={item.id}>{item.label}</option>)}</select></label>
                <label><span className="sr-only">安装状态</span><select value={stateFilter} onChange={event => setStateFilter(event.target.value as MarketStateFilter)}>{stateFilters.map(item => <option key={item.id} value={item.id}>{item.label}</option>)}</select></label>
              </div>
            </div>
            <div className="plugin-catalog-result"><span>{visibleEntries.length} 个结果</span></div>
            <div className="market-list">
              {visibleEntries.map(entry => {
                const key = entry.key;
                const selected = selectedEntry?.key === key;
                return (
                  <button key={key} type="button" className={`market-item${selected ? ' selected' : ''}`} onClick={() => { setSelectedKey(key); setDetailOpen(true); }}>
                    <span className={`extension-kind-badge ${entry.kind}`}>{kindLabels[entry.kind]}</span>
                  <span className="market-item-copy">
                    <strong title={entry.name}>{entry.name}</strong>
                    <small title={entry.description || `${entry.sourceLabel} · ${entry.author}`}>{entry.description || `${entry.sourceLabel} · ${entry.author}`}</small>
                    <small className="catalog-item-author">{entry.sourceLabel} · {entry.author} · v{entry.version}</small>
                  </span>
                    <span className={`skill-state-label ${entry.updateVersion ? 'warn' : entry.installedVersion ? 'success' : 'neutral'}`}>{entry.updateVersion ? '可更新' : entry.installedVersion ? '已安装' : entry.policyLabel}</span>
                  </button>
                );
              })}
              {!loading && !visibleEntries.length ? <EmptyState icon={Search} title="没有匹配的扩展" text={entries.length ? '调整关键词或筛选条件后重试。' : '添加来源后，可安装的扩展会显示在这里。'} /> : null}
            </div>
          </aside>
          <main className="market-detail plugin-catalog-detail">
            <button type="button" className="workspace-back" onClick={() => setDetailOpen(false)}><ArrowLeft size={15} />返回列表</button>
            {selectedEntry ? (
              <MarketDetail
                entry={selectedEntry}
                loadVersions={loadVersions}
                installing={planBusy || installing === selectedBusyKey}
                onInstall={(version) => installEntry(selectedEntry, version)}
                onManage={() => onOpenKind(selectedEntry.kind)}
              />
            ) : <EmptyState icon={Store} title="选择一个扩展" text="查看功能、依赖、版本和安装状态。" />}
          </main>
        </section>
      {pluginPlan || skillPlan || planError ? (
        <MarketPlanDialog
          pluginPlan={pluginPlan}
          skillPlan={skillPlan}
          error={planError}
          busy={installing === selectedBusyKey}
          onClose={() => { setPluginPlan(null); setSkillPlan(null); setPlanError(''); }}
          onInstallPlugin={(item) => { setPluginPlan(null); void runInstall(entryIdentity('plugin', item.plugin_id, item.source || '', item.artifact_id || '', item.sha256 || ''), () => onInstallPlugin(item.plugin_id, item.version, item.source, item.artifact_id, item.sha256)); }}
          onInstallSkill={(item, optionalIds) => { setSkillPlan(null); void runInstall(entryIdentity('skill', item.skill_id, item.source || '', item.artifact_id || '', item.sha256 || ''), () => onInstallSkill(item.skill_id, item.version, optionalIds, item.source, item.artifact_id, item.sha256)); }}
        />
      ) : null}
      <ExtensionSourcesDialog
        open={sourcesOpen}
        workspace={workspace}
        settings={extensionSources}
        snapshot={extensionSourceSnapshot}
        loading={extensionSourcesLoading}
        error={extensionSourcesError}
        onClose={() => setSourcesOpen(false)}
        onSetWorkspace={onSetWorkspace}
        onDevelopWorkspace={onDevelopWorkspace}
        onRefresh={onRefreshSources}
        onAdd={onAddSource}
        onAddLocal={onAddLocalSource}
        onUpdate={onUpdateSourceConfig}
        onRemove={onRemoveSource}
        onSetAcquisition={onSetUnitAcquisition}
        onInstallUnit={onInstallUnit}
      />
    </div>
  );
}

function MarketDetail({ entry, loadVersions, installing, onInstall, onManage }: {
  entry: MarketEntry;
  loadVersions: (entry: MarketEntry) => Promise<MarketVersion[]>;
  installing: boolean;
  onInstall: (version: MarketVersion) => void;
  onManage: () => void;
}) {
  const [versions, setVersions] = useState<MarketVersion[]>([]);
  const [versionsLoading, setVersionsLoading] = useState(false);
  const [versionsError, setVersionsError] = useState('');
  const entryCandidate: MarketVersion = {
    version: entry.version,
    publishedAt: '',
    notes: '',
    source: entry.source,
    sourceLabel: sourceDisplayLabel(entry.source, entry.sourceLabel),
    minAgentVersion: entry.minAgentVersion,
    artifactId: entry.artifactId,
    sha256: entry.sha256,
  };
  const [selectedCandidate, setSelectedCandidate] = useState(versionIdentity(entryCandidate));

  useEffect(() => {
    let active = true;
    setSelectedCandidate(versionIdentity(entryCandidate));
    setVersionsLoading(true);
    setVersionsError('');
    loadVersions(entry)
      .then(list => { if (active) setVersions(list.length ? list : [entryCandidate]); })
      .catch(() => { if (active) { setVersions([entryCandidate]); setVersionsError('暂时无法读取历史版本。'); } })
      .finally(() => { if (active) setVersionsLoading(false); });
    return () => { active = false; };
  }, [entry, loadVersions]);

  const sortedVersions = useMemo(() => [...versions].sort((left, right) => compareSemanticVersions(right.version, left.version)), [versions]);
  const selected = sortedVersions.find(item => versionIdentity(item) === selectedCandidate) || sortedVersions[0];
  const installed = entry.installedVersion;
  const isCurrent = Boolean(installed) && selected?.version === installed;
  const actionLabel = entry.blocked ? '组织已禁止安装' : entry.managed ? '组织管理' : installing ? '正在检查' : isCurrent ? '重新安装此版本' : installed ? '安装此版本' : '安装';
  const disabled = entry.blocked || entry.managed || installing;

  return (
    <>
      <header className="plugin-product-header">
        <div className="plugin-product-title">
          <span className="plugin-product-mark">{entry.name.slice(0, 1)}</span>
          <div>
            <div><h3>{entry.name}</h3><Pill kind={entry.policyKind}>{entry.policyLabel}</Pill><span className={`extension-kind-badge ${entry.kind}`}>{kindLabels[entry.kind]}</span></div>
            <span>{entry.sourceLabel} · {entry.author}</span>
          </div>
        </div>
        <div className="actions-row">
          {installed ? <button type="button" className="btn" onClick={onManage}>管理</button> : null}
          <button type="button" className="btn btn-primary" disabled={disabled || !selected} onClick={() => selected && onInstall(selected)}><Download size={15} />{actionLabel}</button>
        </div>
      </header>
       <p className="plugin-product-description">{entry.description || '暂无说明。'}</p>
      {installed ? <div className="plugin-product-notice"><ShieldCheck size={16} /><div><strong>已安装 v{installed}{entry.updateVersion ? ` · 可更新至 v${entry.updateVersion}` : ''}</strong><span>{entry.kind === 'plugin' ? '可在插件页面启用、停用或回滚。' : entry.kind === 'skill' ? '可在技能页面同步、更新或卸载。' : '可在工作流页面启用、停用或运行。'}</span></div></div> : null}
      <section className="plugin-product-section">
        <div className="plugin-section-heading"><div><h4>版本</h4></div><span className="market-section-note">{versionsLoading ? '读取中' : `${sortedVersions.length} 个可安装版本`}</span></div>
        {versionsError ? <div className="market-inline-warning"><CircleAlert size={14} />{versionsError}</div> : null}
        <div className="market-version-list">
          {sortedVersions.map(candidate => (
            <button type="button" key={versionIdentity(candidate)} className={versionIdentity(candidate) === selectedCandidate ? 'active' : ''} onClick={() => setSelectedCandidate(versionIdentity(candidate))}>
              <span><strong>v{candidate.version}</strong><small>{candidate.version === installed ? `本机已安装 · ${candidate.sourceLabel}` : candidate.minAgentVersion ? `需桌面端 v${candidate.minAgentVersion} · ${candidate.sourceLabel}` : candidate.sourceLabel}</small></span>
              {versionIdentity(candidate) === selectedCandidate ? <CheckCircle2 size={15} /> : null}
            </button>
          ))}
        </div>
        {selected?.notes ? <p className="workflow-market-notes">{selected.notes}</p> : null}
      </section>
      {entry.dependencies.length ? (
        <section className="plugin-product-section">
          <div className="plugin-section-heading"><div><h4>依赖</h4></div></div>
          <div className="plugin-product-dependencies">{entry.dependencies.map(dependency => <div key={dependency.id}><span className="status-dot success" /><span className="plugin-dependency-name"><strong>{dependency.name}</strong></span><span>{dependency.required ? '必需' : '可选'}</span><strong>{dependency.hint}</strong></div>)}</div>
        </section>
      ) : null}
      <section className="plugin-product-section">
        <div className="plugin-section-heading"><div><h4>风险与支持</h4></div></div>
        <div className="plugin-product-dependencies">
          <div><span className="status-dot" /><span>{entry.kind === 'workflow' ? '分发校验' : '权限范围'}</span><span>{kindLabels[entry.kind]}</span><strong>{entry.risk}</strong></div>
          {entry.support.length ? <div><span className="status-dot" /><span>可用范围</span><span>{entry.support.join(' · ')}</span><strong>{entry.minAgentVersion ? `桌面端 v${entry.minAgentVersion}+` : '未限制'}</strong></div> : null}
        </div>
      </section>
      {entry.categories.length ? <section className="plugin-product-section"><div className="plugin-section-heading"><div><h4>分类</h4></div></div><div className="market-taxonomy"><Tags items={functionalCategoryLabels(entry.categories)} /></div></section> : null}
      <details className="plugin-technical-panel">
        <summary>开发者信息</summary>
        <div className="plugin-technical-grid">
          <div><span>稳定 ID</span><code>{entry.id}</code></div>
          <div><span>类型</span><strong>{kindLabels[entry.kind]}</strong></div>
          <div><span>当前版本</span><strong>v{entry.version}</strong></div>
          <div><span>来源</span><strong>{entry.sourceLabel}</strong></div>
          <div><span>制品摘要</span><code>{entry.sha256 ? entry.sha256.slice(0, 12) : '实时目录'}</code></div>
          {entry.artifactId ? <div className="wide"><span>制品 ID</span><code>{entry.artifactId}</code></div> : null}
          <div><span>发布者</span><strong>{entry.author}</strong></div>
          <div><span>最低桌面端版本</span><strong>{entry.minAgentVersion ? `v${entry.minAgentVersion}` : '--'}</strong></div>
          <div className="wide"><span>功能 ID</span><code>{entry.capabilityIds.length ? entry.capabilityIds.join('、') : '--'}</code></div>
        </div>
      </details>
    </>
  );
}

function MarketPlanDialog({ pluginPlan, skillPlan, error, busy, onClose, onInstallPlugin, onInstallSkill }: {
  pluginPlan: PluginInstallPlan | null;
  skillPlan: SkillInstallPlan | null;
  error: string;
  busy: boolean;
  onClose: () => void;
  onInstallPlugin: (item: PluginCatalogItem) => void;
  onInstallSkill: (item: OrganizationSkillCatalogItem, optionalPluginIds: string[]) => void;
}) {
  const [optionalIds, setOptionalIds] = useState<string[]>([]);
  const skillActions = (skillPlan?.plugin_actions || []).map(action => ({ id: action.plugin_id, name: action.plugin_name || readableID(action.plugin_id), required: action.required, action: action.action, targetVersion: action.target_version }));
  const pluginActions = (pluginPlan?.dependency_actions || []).map(action => ({ id: action.plugin_id, name: action.plugin_name || readableID(action.plugin_id), required: true, action: action.action, targetVersion: action.target_version }));
  const actions = skillPlan ? skillActions : pluginActions;
  const target = skillPlan ? { name: skillPlan.skill.name, version: skillPlan.skill.version } : pluginPlan ? { name: pluginPlan.plugin.name, version: pluginPlan.plugin.version } : null;
  const ready = skillPlan ? skillPlan.ready : Boolean(pluginPlan?.ready);
  const title = skillPlan ? '安装技能' : '安装插件';

  return (
    <div className="skill-dialog-backdrop">
      <div className="skill-dialog skill-plan-dialog" role="dialog" aria-modal="true">
        <div className="skill-dialog-head"><strong>{title}</strong><button className="btn btn-icon" onClick={onClose} aria-label="关闭"><X size={16} /></button></div>
        {error ? <div className="skill-dialog-warning"><CircleAlert size={16} />{error}</div> : null}
        {target ? <div className="skill-plan-summary"><strong>{target.name} v{target.version}</strong><span>{ready ? '可以安装' : '当前无法安装'}</span></div> : null}
        {actions.length ? (
          <div className="skill-plan-actions">
            {actions.map(action => (
              <label key={action.id} className={action.action === 'blocked' || action.action === 'unavailable' ? 'blocked' : ''}>
                <input type="checkbox" checked={action.required || optionalIds.includes(action.id)} disabled={action.required || !['install', 'update'].includes(action.action)} onChange={event => setOptionalIds(current => event.target.checked ? [...current, action.id] : current.filter(id => id !== action.id))} />
                <span><strong>{action.name}</strong><small>{planActionDescription(action.action)}</small></span>
                <code>{action.targetVersion ? `v${action.targetVersion}` : '--'}</code>
              </label>
            ))}
          </div>
        ) : <div className="skill-plan-actions"><span className="skill-section-empty">无依赖</span></div>}
        <div className="skill-dialog-actions">
          <button className="btn" onClick={onClose}>取消</button>
          <button className="btn btn-primary" disabled={!ready || busy} onClick={() => { if (skillPlan) onInstallSkill(skillPlan.skill, optionalIds); else if (pluginPlan) onInstallPlugin(pluginPlan.plugin); }}><Download size={15} />确认安装</button>
        </div>
      </div>
    </div>
  );
}

function planActionDescription(action: string) {
  return ({ satisfied: '已安装', install: '将一并安装', update: '将一并更新', blocked: '被组织策略阻止', unavailable: '当前不可用' } as Record<string, string>)[action] || '需要处理';
}
