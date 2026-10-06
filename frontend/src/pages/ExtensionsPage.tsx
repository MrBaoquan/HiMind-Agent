import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  ArrowLeft,
  CircleAlert,
  Download,
  FileText,
  GitBranch,
  Search,
  ShieldCheck,
  Store,
  X,
} from 'lucide-react';
import { EmptyState, PageHeader, Pill, Tags } from '../components/Common';
import { ExtensionKindMark, capabilityKindIcons } from '../components/ExtensionKindMark';
import { ExtensionBatchUpdateDialog } from '../components/ExtensionBatchUpdateDialog';
import { ExtensionSourcesDialog } from '../components/ExtensionSourcesDialog';
import { McpCatalogPanel } from '../components/McpCatalogPanel';
import type { McpManager } from '../components/useMcpManager';
import {
  FUNCTIONAL_CATEGORIES,
  categorySearchText,
  functionalCategoryLabels,
  functionalCategoryMatches,
  resolveFunctionalCategory,
} from '../data/categoryCatalog';
import { capabilityKindLabels, extensionKindLabels, type ExtensionKind, type McpKind } from '../data/extensionKinds';
import {
  compareSemanticVersions,
  entryIdentity,
  friendlySourceName,
  installActionLabel,
  marketVersionFromEntry,
  mergeMarketEntries,
  newerVersion,
  resolveSourceIdentity,
  sourceDisplayLabel,
  sourceNameFor,
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
  InstructionPackCatalogItem,
  OrganizationSkillCatalogItem,
  PluginCatalogItem,
  PluginInstallPlan,
  PluginItem,
  SkillInstallPlan,
  WorkflowCatalogItem,
  WorkflowCenterItem,
  ExpertSummary,
  ExpertCatalogItem,
} from '../services/agentApi';

type MarketKind = ExtensionKind | McpKind | 'instruction';
const marketKinds: MarketKind[] = ['plugin', 'skill', 'workflow', 'expert', 'instruction', 'mcp'];
const marketKindLabels: Record<Exclude<MarketKind, 'mcp'>, string> = { ...extensionKindLabels, instruction: '项目规则' };

type MarketStateFilter = 'all' | 'available' | 'installed' | 'update';

/// 市场页 Banner 按模块分列。五类目录的错误合并成一段文本时，用户只知道"坏了"，
/// 不知道坏的是插件、技能还是工作流，也就没法判断"别的还能不能用"。
export type MarketLoadError = { module: string; message: string };

/// One discoverable extension. Plugin, Skill and Workflow catalogs stay
/// separate on the wire; this is the read model the page renders so the three
/// kinds can be searched and compared side by side.
type MarketEntry = MarketProduct & {
  kind: Exclude<MarketKind, 'mcp'>;
  name: string;
  description: string;
  author: string;
  risk: string;
  plugin?: PluginCatalogItem;
  skill?: OrganizationSkillCatalogItem;
  workflow?: WorkflowCatalogItem;
  expert?: ExpertCatalogItem;
  instruction?: InstructionPackCatalogItem;
};

type MarketVersion = MarketCandidate;

/// 首次进入「市场」时三类目录可能仍在构建扩展源快照，按这个节奏重试。
const CATALOG_RETRY_DELAYS_MS = [0, 4000, 10000, 20000, 45000, 90000];

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

function formatPublishedAt(value?: string) {
  if (!value) return '';
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleDateString('zh-CN');
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
  // 没有组织策略就不给标签：详情头部挂一个「可选安装」，读起来像个状态，
  // 实际只是「这条不是组织配发的」，属于把默认值当信息讲。
  return '';
}

function assignmentKind(label: string): MarketEntry['policyKind'] {
  if (label === '组织已禁止') return 'danger';
  if (label === '组织必装' || label === '组织推荐') return 'warn';
  return 'neutral';
}

/// 列表行角标只回答一件事：这个扩展现在能不能用、要不要动它。
/// 口径与筛选项一致（未安装 / 已安装 / 可更新），组织策略（必装、禁止）
/// 因为会改变可用性，优先于安装状态展示。
function marketStateBadge(entry: MarketEntry): { label: string; tone: 'success' | 'warn' | 'danger' | 'neutral' } {
  if (entry.blocked) return { label: entry.policyLabel || '组织已禁止', tone: 'danger' };
  if (updatableVersion(entry)) return { label: '可更新', tone: 'warn' };
  if (entry.installedVersion) return { label: '已安装', tone: 'success' };
  if (entry.policyLabel === '组织必装' || entry.policyLabel === '组织推荐') return { label: entry.policyLabel, tone: 'warn' };
  return { label: entry.kind === 'instruction' ? '可导入' : '未安装', tone: 'neutral' };
}

/// 组织禁止或组织管理的扩展，市场里不该宣传「可更新」：详情页的安装按钮本来就是
/// 禁用的，批量更新也会把它们排除在外。角标、筛选、「全部更新」计数三处统一走这里，
/// 避免出现「列表说有 5 项可更新，点进去只有 4 项能动手」。
function updatableVersion(entry: MarketEntry) {
  if (!entry.updateVersion || entry.blocked || entry.managed) return '';
  return entry.updateVersion;
}

/// 列表行显示的版本：有更新就给更新目标（那是这个产品当下最需要被看见的版本），
/// 本机装了就给本机那一版，都没装才给目录版本。多来源时目录版本是聚合后的头部版本，
/// 但本机可能装得比它更新，所以这里仍然先认本机版本，避免行里写出「已安装 v1.0.1」
/// 而本机其实是 v1.0.2 这种自相矛盾的状态。
function displayVersion(entry: MarketEntry) {
  return updatableVersion(entry) || entry.installedVersion || entry.version;
}

type BuildInput = {
  plugins: PluginCatalogItem[];
  skills: OrganizationSkillCatalogItem[];
  workflows: WorkflowCatalogItem[];
  installedPlugins: PluginItem[];
  installedSkills: CodexSkillStatusItem[];
  installedWorkflows: WorkflowCenterItem[];
  experts: ExpertSummary[];
  expertCatalog: ExpertCatalogItem[];
  instructionPacks: InstructionPackCatalogItem[];
  /// 扩展源配置里的「ID → 名字」，用来把 local:xxx / github:xxx 换成用户认识的名字。
  sourceNames: Map<string, string>;
};

/// 来源名优先用「来源管理」里配置的名字；没有配置项的来源（系统内置、工作台下发、
/// 随手导入的本机目录）按产品叫法兜底，不退回到"本地源码/组织发布"这类类型词。
function sourceNameOf(source: { group: MarketSourceGroup; id: string }, nameById: Map<string, string>) {
  const configured = source.id ? nameById.get(source.id) || '' : '';
  return friendlySourceName(configured, '') || sourceNameFor(source.group);
}

function buildEntries({ plugins, skills, workflows, installedPlugins, installedSkills, installedWorkflows, experts, expertCatalog, instructionPacks, sourceNames: sourceNameById }: BuildInput): MarketEntry[] {
  const installedPluginById = new Map(installedPlugins.map(item => [item.id, item]));
  const installedSkillById = new Map(installedSkills.map(item => [item.record.manifest.id, item]));
  const installedWorkflowById = new Map(installedWorkflows.map(item => [item.package.id, item]));
  const installedExpertById = new Map(experts.map(item => [item.id, item]));
  const pluginNameById = new Map(plugins.map(item => [item.plugin_id, item.name || item.plugin_id]));

  const pluginEntries = plugins.map<MarketEntry>(item => {
    const installed = installedPluginById.get(item.plugin_id);
    const source = resolveSourceIdentity(item.source);
    const sourceName = sourceNameOf(source, sourceNameById);
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
      sourceId: source.id,
      sourceName,
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
    const source = resolveSourceIdentity(item.source, item.channel);
    const sourceName = sourceNameOf(source, sourceNameById);
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
      sourceId: source.id,
      sourceName,
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
    const source = resolveSourceIdentity(item.source, item.channel);
    const sourceName = sourceNameOf(source, sourceNameById);
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
      sourceId: source.id,
      sourceName,
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

  const expertEntries = expertCatalog.map<MarketEntry>(item => {
    const installed = installedExpertById.get(item.expert_id);
    const source = resolveSourceIdentity(item.source);
    const sourceName = sourceNameOf(source, sourceNameById);
    const policy = assignmentLabel(item.assignment, undefined, false);
    return {
      key: entryIdentity('expert', item.expert_id, item.source || '', item.artifact_id || '', item.sha256 || ''),
      kind: 'expert', id: item.expert_id, name: item.name || item.expert_id,
      description: item.description || '', author: item.author_name || '发布者未注明', version: item.version,
      categories: item.categories || [], capabilityIds: [], dependencies: [], sourceGroup: source.group,
      sourceLabel: source.label, sourceId: source.id, sourceName, policyLabel: policy, policyKind: assignmentKind(policy),
      blocked: item.assignment === 'blocked', managed: Boolean(item.managed), installedVersion: installed?.version || '',
      updateVersion: installed ? newerVersion(item.version, installed.version) : '', risk: '会话工作方法',
      support: item.supported_clients || [], minAgentVersion: '', source: item.source || '', artifactId: item.artifact_id || '', sha256: item.sha256 || '', expert: item,
    };
  });
  const instructionEntries = instructionPacks.map<MarketEntry>(item => {
    const source = resolveSourceIdentity(item.source, item.channel);
    const sourceName = sourceNameOf(source, sourceNameById);
    const policy = assignmentLabel(item.assignment, undefined, false);
    return {
      key: entryIdentity('instruction', item.instruction_pack_id, item.source || '', item.artifact_id || '', item.sha256 || ''),
      kind: 'instruction', id: item.instruction_pack_id, name: item.name || item.instruction_pack_id,
      description: item.description || '', author: item.author_name || '发布者未注明', version: item.version,
      categories: item.categories || [], capabilityIds: [], dependencies: [], sourceGroup: source.group,
      sourceLabel: source.label, sourceId: source.id, sourceName, policyLabel: policy, policyKind: assignmentKind(policy),
      blocked: item.assignment === 'blocked', managed: Boolean(item.managed), installedVersion: '', updateVersion: '',
      risk: '项目上下文', support: item.supported_clients || [], minAgentVersion: item.min_agent_version || '', source: item.source || '', artifactId: item.artifact_id || '', sha256: item.sha256 || '', instruction: item,
    };
  });
  return mergeMarketEntries([...pluginEntries, ...skillEntries, ...workflowEntries, ...expertEntries, ...instructionEntries]);
}

function entrySearchText(entry: MarketEntry) {
  return [
    entry.name,
    entry.id,
    entry.description,
    entry.author,
    entry.sourceName || entry.sourceLabel,
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

export function ExtensionsPage({
  loading,
  errors,
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
  installTargets,
  onPickSkillLocation,
  onLoadPluginVersions,
  onLoadSkillVersions,
  onLoadWorkflowVersions,
  onInstallWorkflow,
  instructionPacks,
  onInstallInstructionPack,
  experts,
  expertCatalog,
  activeExpert,
  onRefreshExperts,
  onActivateExpert,
  onNotify,
  onInstallMarketExpert,
  workspace,
  extensionSources,
  extensionSourceSnapshot,
  extensionSourcesLoading,
  extensionSourcesError,
  onRefreshSources,
  onAddSource,
  onUpdateSourceConfig,
  onRemoveSource,
  onSetUnitAcquisition,
  onDevelopWorkspace,
  openSourcesRequest,
  onSourcesRequestHandled,
  mcp,
  openMcpRequest,
  onMcpRequestHandled,
  onManageMcp,
  onBatchUpdateFinished,
}: {
  loading: boolean;
  errors: MarketLoadError[];
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
  onInstallSkill: (skillId: string, version: string | undefined, optionalPluginIds: string[], source?: string, artifactId?: string, sha256?: string, clients?: string[], location?: string) => Promise<void>;
  installTargets: { id: string; name: string; detected: boolean }[];
  onPickSkillLocation: () => Promise<string>;
  onLoadPluginVersions: (pluginId: string, source?: string) => Promise<PluginCatalogItem[]>;
  onLoadSkillVersions: (skillId: string, source?: string) => Promise<OrganizationSkillCatalogItem[]>;
  onLoadWorkflowVersions: (workflowId: string, source?: string) => Promise<WorkflowCatalogItem[]>;
  onInstallWorkflow: (workflowId: string, version: string, source: string, artifactId?: string, sha256?: string) => Promise<void>;
  instructionPacks: InstructionPackCatalogItem[];
  onInstallInstructionPack: (id: string, version?: string, artifactId?: string, sha256?: string) => Promise<void>;
  experts: ExpertSummary[];
  expertCatalog: ExpertCatalogItem[];
  activeExpert: string;
  onRefreshExperts: () => Promise<void> | void;
  onActivateExpert: (id: string, version: string) => Promise<void>;
  onNotify: (message: string, tone?: 'success' | 'error') => void;
  onInstallMarketExpert: (item: ExpertCatalogItem) => Promise<void>;
  workspace: ExtensionWorkspaceSettings;
  extensionSources: ExtensionSourceSettings;
  extensionSourceSnapshot: ExtensionSourceSnapshot | null;
  extensionSourcesLoading: boolean;
  extensionSourcesError: string;
  onRefreshSources: () => Promise<void>;
  onAddSource: (name: string, repository: string, reference: string, catalogPath: string, verification: ExtensionSourceConfig['verification']) => Promise<void>;
  onUpdateSourceConfig: (source: ExtensionSourceConfig, enabled: boolean, autoUpdate: boolean, verification: ExtensionSourceConfig['verification']) => Promise<void>;
  onRemoveSource: (sourceId: string) => Promise<void>;
  onSetUnitAcquisition: (unitKey: string, acquisition: ExtensionSourceAcquisition) => Promise<void>;
  onDevelopWorkspace: (root: string) => void;
  openSourcesRequest: number;
  /// 打开来源管理的请求是一次性的：消费掉之后要清零，否则每次重新进入市场都会
  /// 再把对话框弹出来。
  onSourcesRequestHandled: () => void;
  /// MCP 工具是接进来的本机工具，没有制品与版本，所以不走 `entries` 那条目录口径，
  /// 单独读一份连接状态；市场和「我的能力」两处的启停、编辑都在这里收口。
  mcp: McpManager;
  /// 「浏览 MCP 工具」这类跨页入口要能直接把市场切到 MCP 页签，同样是一次性请求。
  openMcpRequest: number;
  onMcpRequestHandled: () => void;
  onManageMcp: () => void;
  /// 批量更新动过版本之后，市场清单与「我的能力」的安装状态都得重新取一遍。
  onBatchUpdateFinished: () => void | Promise<void>;
}) {
  const [kindFilter, setKindFilter] = useState<'all' | MarketKind>('all');
  // 来源筛选按真实来源名的稳定键走（配置源用来源 ID），不再用"本地源码/组织发布"这类类型词。
  const [sourceFilter, setSourceFilter] = useState<string>('all');
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
  const [batchOpen, setBatchOpen] = useState(false);
  const catalogAttempts = useRef(0);
  // `onRefresh` is a fresh closure on every parent render; keeping it in a ref
  // stops the retry effect from resetting its timer before it can fire.
  const refreshCatalogs = useRef(onRefresh);
  refreshCatalogs.current = onRefresh;

  useEffect(() => {
    if (openSourcesRequest > 0) {
      setSourcesOpen(true);
      onSourcesRequestHandled();
    }
  }, [openSourcesRequest, onSourcesRequestHandled]);

  useEffect(() => {
    if (openMcpRequest > 0) {
      setKindFilter('mcp');
      onMcpRequestHandled();
    }
  }, [openMcpRequest, onMcpRequestHandled]);

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

  // 自动登记的本机来源会把绝对路径当作名字，这里先收敛成目录名再交给列表使用。
  const sourceNameById = useMemo(
    () => new Map((extensionSourceSnapshot?.sources || []).map(item => [item.source.id, friendlySourceName(item.source.name, item.source.repository)])),
    [extensionSourceSnapshot],
  );

  const entries = useMemo(
    () => buildEntries({ plugins, skills, workflows, installedPlugins, installedSkills, installedWorkflows, experts, expertCatalog, instructionPacks, sourceNames: sourceNameById }),
    [expertCatalog, experts, instructionPacks, installedPlugins, installedSkills, installedWorkflows, plugins, skills, sourceNameById, workflows],
  );

  /// 来源卡片的「待更新」必须和市场列表同一个口径，所以直接把市场算好的更新目标
  /// 版本交给它：卡片只负责回答「这条更新是不是我这个来源提供的」。
  /// 组织管理与组织禁止的制品版本由组织推进，`updatableVersion` 已把它们排除，
  /// 卡片自然也不再统计。
  const unitUpdateTargets = useMemo(
    () => new Map(
      entries
        .map(entry => [`${entry.kind}:${entry.id}`, updatableVersion(entry)] as [string, string])
        .filter(([, version]) => Boolean(version)),
    ),
    [entries],
  );

  /// 来源下拉只列出当前目录里真实出现过的来源，用它们的名字；空来源不会占一个选项。
  const sourceOptions = useMemo(() => {
    const options = new Map<string, string>();
    for (const entry of entries) {
      const keys = entry.sourceKeys || [];
      const names = entry.sourceNames || [];
      keys.forEach((key, index) => { if (!options.has(key)) options.set(key, names[index] || key); });
    }
    return [...options.entries()].map(([id, label]) => ({ id, label }));
  }, [entries]);

  // 来源列表会随目录刷新变化（例如某个来源被停用后整批下架）：选中的来源消失时要回到全部，
  // 否则下拉会停在空白上，用户看不出当前在看什么。
  useEffect(() => {
    if (sourceFilter !== 'all' && !sourceOptions.some(option => option.id === sourceFilter)) setSourceFilter('all');
  }, [sourceFilter, sourceOptions]);

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
      if (sourceFilter !== 'all' && !(entry.sourceKeys || []).includes(sourceFilter)) return false;
      if (categoryFilter === 'uncategorized') { if (!isUncategorized(entry)) return false; }
      else if (categoryFilter !== 'all' && !functionalCategoryMatches(entry.categories, categoryFilter)) return false;
      if (stateFilter === 'available' && entry.installedVersion) return false;
      if (stateFilter === 'installed' && !entry.installedVersion) return false;
      if (stateFilter === 'update' && !updatableVersion(entry)) return false;
      if (normalized && !entrySearchText(entry).includes(normalized)) return false;
      return true;
    });
  }, [categoryFilter, entries, kindFilter, query, sourceFilter, stateFilter]);
  const kindCounts = useMemo(() => Object.fromEntries((['plugin', 'skill', 'workflow', 'expert', 'instruction'] as const).map(kind => [kind, entries.filter(entry => entry.kind === kind).length])) as Record<'plugin' | 'skill' | 'workflow' | 'expert' | 'instruction', number>, [entries]);

  const selectedEntry = visibleEntries.find(entry => entry.key === selectedKey) || visibleEntries[0] || null;
  const availableCount = entries.filter(entry => !entry.installedVersion).length;
  const updateCount = entries.filter(entry => updatableVersion(entry)).length;
  const selectedBusyKey = selectedEntry?.key || '';

  const loadVersions = useCallback(async (entry: MarketEntry): Promise<MarketVersion[]> => {
    const sources = [...new Set((entry.sourceCandidates || [marketVersionFromEntry(entry)]).map(candidate => candidate.source))];
    const results = await Promise.allSettled(sources.map(async source => {
      const sourceArg = source || undefined;
      if (entry.kind === 'plugin') return onLoadPluginVersions(entry.id, sourceArg);
      if (entry.kind === 'skill') return onLoadSkillVersions(entry.id, sourceArg);
      if (entry.kind === 'workflow') return onLoadWorkflowVersions(entry.id, sourceArg);
      return [];
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
    if (entry.kind === 'workflow') { void runInstall(entry.key, () => onInstallWorkflow(entry.id, candidate.version, candidate.source, candidate.artifactId, candidate.sha256)); return; }
    if (entry.kind === 'expert' && entry.expert) { void runInstall(entry.key, () => onInstallMarketExpert(entry.expert!)); return; }
    if (entry.kind === 'instruction' && entry.instruction) { void runInstall(entry.key, () => onInstallInstructionPack(entry.id, candidate.version, candidate.artifactId, candidate.sha256)); }
  }

  return (
    <div className="plugin-page market-page">
      <PageHeader
        title="市场"
        actions={<button className="btn" title="管理来源" onClick={() => setSourcesOpen(true)}><GitBranch size={14} />来源管理</button>}
      />
      {errors.length ? <div className="blocker market-load-errors"><CircleAlert size={18} /><div><strong>{errors.length === 1 ? `${errors[0].module}目录读取失败` : `部分目录数据读取失败（${errors.map(item => item.module).join('、')}）`}</strong>{errors.map(item => <span key={item.module}>{item.module}：{item.message}</span>)}</div></div> : null}
        {/* MCP 工具页签没有列表可铺，主从两栏会空掉半屏，所以整页切成单栏（mcp-mode）。 */}
        <section className={`market-workspace${kindFilter === 'mcp' ? ' mcp-mode' : ' compact-master-detail'}${detailOpen ? ' detail-open' : ''}`}>
          {/* 类型页签横跨两栏：它管的是整个市场的范围，不是左列表的筛选条件。
              挤在 43% 的左列里，五类能力就会折行，右半边还空着。 */}
          <div className="market-kind-bar">
            <div className="plugin-tabs market-kind-tabs" role="tablist" aria-label="扩展类型">
              <button role="tab" aria-selected={kindFilter === 'all'} className={kindFilter === 'all' ? 'active' : ''} onClick={() => { setKindFilter('all'); setDetailOpen(false); }}>全部 <span>{entries.length}</span></button>
              {marketKinds.map(kind => {
                const KindIcon = kind === 'instruction' ? FileText : capabilityKindIcons[kind];
                // 市场页签上的数字是「目录里有多少可获得的」，不是「已经装了几条」。
                const count = kind === 'mcp' ? mcp.catalog.entries.length : kindCounts[kind as keyof typeof kindCounts] || 0;
                return <button role="tab" key={kind} aria-selected={kindFilter === kind} className={kindFilter === kind ? 'active' : ''} onClick={() => { setKindFilter(kind); setDetailOpen(false); }}><KindIcon size={14} />{kind === 'mcp' ? capabilityKindLabels[kind] : marketKindLabels[kind]} <span>{count}</span></button>;
              })}
            </div>
          </div>
          <aside className="market-browser">
            <div className="market-tools">
              {/* MCP 页签搜的是工具，不是「能力」：同一句占位文案跨页签复用会让人以为这里能搜到插件和技能。 */}
              <label className="plugin-search"><Search size={15} /><span className="sr-only">{kindFilter === 'mcp' ? '搜索 MCP 工具' : '搜索扩展'}</span><input value={query} onChange={event => setQuery(event.target.value)} placeholder={kindFilter === 'mcp' ? '搜索 MCP 工具名称或用途' : '搜索名称、用途或能力'} /></label>
              {/* MCP 工具没有功能分类、来源与安装状态这几层筛选，页签下面就不再铺一排用不上的控件。 */}
              {kindFilter === 'mcp' ? null : <div className="market-category-block">
                <label className="market-category-select">
                  <span className="sr-only">功能分类</span>
                  <select value={categoryFilter} onChange={event => setCategoryFilter(event.target.value)}>
                    <option value="all">全部分类（{entries.length}）</option>
                    {FUNCTIONAL_CATEGORIES.map(category => <option key={category.id} value={category.id}>{category.label}（{categoryCounts.get(category.id) || 0}）</option>)}
                    <option value="uncategorized">未分类（{uncategorizedCount}）</option>
                  </select>
                </label>
                <nav className="market-category-nav" aria-label="扩展功能分类">
                  <button type="button" className={categoryFilter === 'all' ? 'active' : ''} onClick={() => setCategoryFilter('all')}>全部<span>{entries.length}</span></button>
                  {FUNCTIONAL_CATEGORIES.map(category => <button type="button" key={category.id} className={categoryFilter === category.id ? 'active' : ''} onClick={() => setCategoryFilter(category.id)}>{category.label}<span>{categoryCounts.get(category.id) || 0}</span></button>)}
                  <button type="button" className={categoryFilter === 'uncategorized' ? 'active' : ''} onClick={() => setCategoryFilter('uncategorized')}>未分类<span>{uncategorizedCount}</span></button>
                </nav>
              </div>}
              {kindFilter === 'mcp' ? null : <div className="market-refine">
                <label><span className="sr-only">扩展来源</span><select value={sourceFilter} onChange={event => setSourceFilter(event.target.value)}><option value="all">全部来源</option>{sourceOptions.map(item => <option key={item.id} value={item.id}>{item.label}</option>)}</select></label>
                <label><span className="sr-only">安装状态</span><select value={stateFilter} onChange={event => setStateFilter(event.target.value as MarketStateFilter)}>{stateFilters.map(item => <option key={item.id} value={item.id}>{item.label}</option>)}</select></label>
              </div>}
            </div>
            {kindFilter === 'mcp' ? null : <>
            <div className="plugin-catalog-result">
              <span>{visibleEntries.length} 个扩展</span>
              <span className="market-result-meta">
                {updateCount ? <button type="button" className={`market-update-chip${stateFilter === 'update' ? ' active' : ''}`} onClick={() => setStateFilter(stateFilter === 'update' ? 'all' : 'update')} title="只显示有可用更新的扩展">{updateCount} 项可更新</button> : null}
                {updateCount ? <button type="button" className="market-update-action" onClick={() => setBatchOpen(true)} title="核对来源后批量更新扩展"><Download size={12} />全部更新</button> : null}
              </span>
            </div>
            <div className="market-list">
              {visibleEntries.map(entry => {
                const key = entry.key;
                const selected = selectedEntry?.key === key;
                const badge = marketStateBadge(entry);
                return (
                  <button key={key} type="button" className={`market-item${selected ? ' selected' : ''}`} onClick={() => { setSelectedKey(key); setDetailOpen(true); }}>
                    {/* 列表行的类型只靠这一格认：图标 + 颜色，文案交给页签和详情头部。 */}
                    {entry.kind === 'instruction' ? <span className="extension-kind-mark instruction" role="img" aria-label="项目规则" title="项目规则"><FileText size={19} strokeWidth={1.8} /></span> : <ExtensionKindMark kind={entry.kind} label={marketKindLabels[entry.kind]} />}
                  <span className="market-item-copy">
                    <strong title={entry.name}>{entry.name}</strong>
                    <small title={entry.description || `${entry.sourceName} · ${entry.author}`}>{entry.description || `${entry.sourceName} · ${entry.author}`}</small>
                    <small className="catalog-item-author">{entry.sourceName} · {entry.author} · v{displayVersion(entry)}</small>
                  </span>
                    <span className={`skill-state-label ${badge.tone}`}>{badge.label}</span>
                  </button>
                );
              })}
              {!loading && !visibleEntries.length ? <EmptyState icon={Search} title="没有匹配的扩展" text={entries.length ? '调整关键词或筛选条件后重试。' : '添加来源后，可安装的扩展会显示在这里。'} /> : null}
            </div>
            </>}
          </aside>
          <main className="market-detail plugin-catalog-detail">
            {kindFilter === 'mcp' ? (
              // MCP 工具没有版本、依赖与制品签名，详情页那一套用不上，直接把获得面板铺上来。
              <McpCatalogPanel mcp={mcp} query={query} onManage={onManageMcp} />
            ) : (
              <>
                <button type="button" className="workspace-back" onClick={() => setDetailOpen(false)}><ArrowLeft size={15} />返回列表</button>
                {selectedEntry ? (
                  <MarketDetail
                    entry={selectedEntry}
                    loadVersions={loadVersions}
                    installing={planBusy || installing === selectedBusyKey}
                    onInstall={(version) => installEntry(selectedEntry, version)}
                    onManage={() => { if (selectedEntry.kind !== 'instruction') onOpenKind(selectedEntry.kind); }}
                  />
                ) : <EmptyState icon={Store} title="选择一个扩展" text="查看功能、依赖、版本和安装状态。" />}
              </>
            )}
          </main>
        </section>
      {pluginPlan || skillPlan || planError ? (
        <MarketPlanDialog
          pluginPlan={pluginPlan}
          skillPlan={skillPlan}
          installTargets={installTargets}
          onPickLocation={onPickSkillLocation}
          error={planError}
          busy={installing === selectedBusyKey}
          onClose={() => { setPluginPlan(null); setSkillPlan(null); setPlanError(''); }}
          onInstallPlugin={(item) => { setPluginPlan(null); void runInstall(entryIdentity('plugin', item.plugin_id, item.source || '', item.artifact_id || '', item.sha256 || ''), () => onInstallPlugin(item.plugin_id, item.version, item.source, item.artifact_id, item.sha256)); }}
          onInstallSkill={(item, optionalIds, clients, location) => { setSkillPlan(null); void runInstall(entryIdentity('skill', item.skill_id, item.source || '', item.artifact_id || '', item.sha256 || ''), () => onInstallSkill(item.skill_id, item.version, optionalIds, item.source, item.artifact_id, item.sha256, clients, location)); }}
        />
      ) : null}
      <ExtensionBatchUpdateDialog
        open={batchOpen}
        onClose={() => setBatchOpen(false)}
        onFinished={onBatchUpdateFinished}
      />
      <ExtensionSourcesDialog
        open={sourcesOpen}
        workspace={workspace}
        settings={extensionSources}
        snapshot={extensionSourceSnapshot}
        unitUpdateTargets={unitUpdateTargets}
        loading={extensionSourcesLoading}
        error={extensionSourcesError}
        onClose={() => setSourcesOpen(false)}
        onDevelopWorkspace={onDevelopWorkspace}
        onRefresh={onRefreshSources}
        onAdd={onAddSource}
        onUpdate={onUpdateSourceConfig}
        onRemove={onRemoveSource}
        onSetAcquisition={onSetUnitAcquisition}
        onInstallUnit={onInstallUnit}
      />
    </div>
  );
}

function InstructionPackMarketPanel({ items, query, onInstall }: {
  items: InstructionPackCatalogItem[];
  query: string;
  onInstall: (id: string, version?: string, artifactId?: string, sha256?: string) => Promise<void>;
}) {
  const normalized = query.trim().toLowerCase();
  const visible = items.filter(item => !normalized || `${item.name} ${item.instruction_pack_id} ${item.description} ${item.author_name} ${item.categories.join(' ')}`.toLowerCase().includes(normalized));
  return <section className="block instruction-pack-market-panel">
    <div className="plugin-section-heading"><div><h3>项目规则</h3><span>可复用的项目说明和输出要求。导入后在扩展开发的规则库中确认，再按需同步到客户端。</span></div><Pill kind="neutral">可复用资产</Pill></div>
    <div className="market-list">
      {visible.map(item => <article className="extension-version-row" key={`${item.instruction_pack_id}:${item.version}`}>
        <div className="extension-version-main"><div><strong>{item.name}</strong><Pill kind="neutral">v{item.version}</Pill></div><time>{item.author_name || '发布者未注明'} · {item.scope || 'project'} · {item.supported_clients.join('、')}</time><p>{item.description}</p></div>
        <button type="button" className="btn btn-primary" onClick={() => void onInstall(item.instruction_pack_id, item.version, item.artifact_id, item.sha256)}><Download size={14} />导入草稿</button>
      </article>)}
      {!visible.length ? <EmptyState icon={Search} title={items.length ? '没有匹配的项目规则' : '暂无可用项目规则'} text={items.length ? '调整关键词后重试。' : '已发布并通过审核的项目规则会显示在这里。'} /> : null}
    </div>
  </section>;
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
  // 顶部按钮装的是「这个产品当下最该装的版本」：有可更新版本时就是那个新版本，
  // 否则才退回目录主来源的版本。同一产品有多个来源时主来源可能比本机已装的还旧，
  // 直接取主来源版本会出现「列表写着可更新至 v1.0.2，按钮却写着降级到 v1.0.1」。
  const recommendedVersion = updatableVersion(entry) || entry.version;
  const entryCandidate: MarketVersion = {
    version: recommendedVersion,
    publishedAt: '',
    notes: '',
    source: entry.source,
    sourceLabel: sourceDisplayLabel(entry.source, entry.sourceLabel),
    sourceName: entry.sourceName || sourceDisplayLabel(entry.source, entry.sourceLabel),
    minAgentVersion: entry.minAgentVersion,
    artifactId: entry.artifactId,
    sha256: entry.sha256,
  };

  useEffect(() => {
    let active = true;
    setVersionsLoading(true);
    setVersionsError('');
    loadVersions(entry)
      .then(list => { if (active) setVersions(list.length ? list : [entryCandidate]); })
      .catch(() => { if (active) { setVersions([entryCandidate]); setVersionsError('暂时无法读取历史版本。'); } })
      .finally(() => { if (active) setVersionsLoading(false); });
    return () => { active = false; };
  }, [entry, loadVersions]);

  const sortedVersions = useMemo(() => [...versions].sort((left, right) => compareSemanticVersions(right.version, left.version)), [versions]);
  const installed = entry.installedVersion;
  // 顶部按钮装的是目录里的推荐版本；历史版本在下面的清单里逐行直接装，
  // 所以按钮必须带上版本号，"安装此版本"这种说法看不出会装成哪一个。
  const lockingLabel = entry.blocked ? '组织已禁止安装' : entry.managed ? '组织管理' : '';
  const actionLabel = installing ? '正在处理' : entry.kind === 'instruction'
    ? '导入到规则库'
    : installActionLabel({ target: entryCandidate.version, installed, locked: lockingLabel });
  const disabled = entry.blocked || entry.managed || installing;
  // 降级不该是详情页最显眼的那个动作，只有安装和升级才用主按钮样式，
  // 与下方版本清单「本机就是这一版就退回普通样式」的规则保持一致。
  const headerPrimary = !installed || compareSemanticVersions(entryCandidate.version, installed) > 0;
  // 本机已装、且没有任何来源给出更高版本时，这个产品对用户的状态是「已经拥有」，
  // 头部再放一个安装类按钮只会重复版本清单里的动作，所以这里换成一句状态。
  // 组织已禁止/组织管理的产品例外：那个按钮是在解释「为什么装不了」。
  const upToDate = !!installed && !updatableVersion(entry) && compareSemanticVersions(entryCandidate.version, installed) <= 0;
  const showHeaderInstall = !upToDate || !!lockingLabel;

  return (
    <>
      <header className="plugin-product-header">
        <div className="plugin-product-title">
          {entry.kind === 'instruction' ? <span className="extension-kind-mark instruction" aria-hidden="true"><FileText size={19} strokeWidth={1.8} /></span> : <ExtensionKindMark kind={entry.kind} />}
          <div>
            <div><h3>{entry.name}</h3>{entry.policyLabel ? <Pill kind={entry.policyKind}>{entry.policyLabel}</Pill> : null}</div>
            {/* 类型在详情头部走文字，形状交给左边那一格，两个彩色小块并排会互相打架。 */}
            <span>{marketKindLabels[entry.kind]} · {entry.sourceName} · {entry.author}</span>
          </div>
        </div>
        <div className="actions-row">
          {/* 「管理」单独出现时看不出管的是什么；这个按钮实际跳去「我的能力」的对应页签。 */}
          {installed ? <button type="button" className="btn" title="在「我的能力」里管理" onClick={onManage}>管理已安装</button> : null}
          {upToDate && !lockingLabel ? <Pill kind="success">已是最新</Pill> : null}
          {showHeaderInstall ? <button type="button" className={`btn${headerPrimary ? ' btn-primary' : ''}`} disabled={disabled} onClick={() => onInstall(entryCandidate)}><Download size={15} />{actionLabel}</button> : null}
        </div>
      </header>
      {entry.description ? <p className="plugin-product-description">{entry.description}</p> : null}
      {installed ? <div className="plugin-product-notice"><ShieldCheck size={16} /><div><strong>已安装 v{installed}{updatableVersion(entry) ? ` · 可更新至 v${entry.updateVersion}` : entry.managed ? ' · 版本由组织推进' : ''}</strong><span>{entry.kind === 'plugin' ? '可在插件页面启用或停用。' : entry.kind === 'skill' ? '可在技能页面同步、更新或卸载。' : entry.kind === 'workflow' ? '可在工作流页面启用、停用或运行。' : entry.kind === 'expert' ? '可在“我的能力 → 专家”中选择和切换。' : '已导入规则库，可在项目中选择。'}</span></div></div> : null}
      <section className="plugin-product-section">
        <div className="plugin-section-heading"><div><h4>版本</h4></div><span className="market-section-note">{versionsLoading ? '读取中' : `${sortedVersions.length} 个可安装版本`}</span></div>
        {versionsError ? <div className="market-inline-warning"><CircleAlert size={14} />{versionsError}</div> : null}
        <div className="market-version-list">
          {sortedVersions.map(candidate => <article className="extension-version-row" key={versionIdentity(candidate)}>
            <div className="extension-version-main">
              <div><strong>v{candidate.version}</strong>{candidate.version === installed ? <Pill kind="success">本机已安装</Pill> : null}</div>
              <time>{[formatPublishedAt(candidate.publishedAt), candidate.sourceName || candidate.sourceLabel, candidate.minAgentVersion ? `需桌面端 v${candidate.minAgentVersion}` : ''].filter(Boolean).join(' · ')}</time>
              {candidate.notes ? <p>{candidate.notes}</p> : null}
            </div>
            {/* 本机已经是这一版时不再给按钮：行首的「本机已安装」已经说明结果，
                「重新安装 vX」既像补丁，也不是这页推荐的动作；换来源重装走来源管理。
                其余版本行只有安装和升级用主按钮样式，降级是显式选择。 */}
            {candidate.version === installed ? null : <button type="button" className={compareSemanticVersions(candidate.version, installed) > 0 ? 'btn btn-primary' : 'btn'} disabled={disabled} onClick={() => onInstall(candidate)}>{entry.kind === 'instruction' ? '导入到规则库' : installActionLabel({ target: candidate.version, installed, locked: lockingLabel })}</button>}
          </article>)}
        </div>
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
          {/* 标签列只留一句「这一行讲什么」，类型已经在详情头部（插件 · 来源 · 作者）写过，
              这里再放一次「插件」会被读成权限的一部分。 */}
          <div><span className="status-dot" /><span>{entry.kind === 'workflow' ? '分发校验' : '权限范围'}</span><strong>{entry.risk}</strong></div>
          {entry.support.length ? <div><span className="status-dot" /><span>可用范围</span><span>{entry.support.join(' · ')}</span><strong>{entry.minAgentVersion ? `桌面端 v${entry.minAgentVersion}+` : '未限制'}</strong></div> : null}
        </div>
      </section>
      {entry.categories.length ? <section className="plugin-product-section"><div className="plugin-section-heading"><div><h4>分类</h4></div></div><div className="market-taxonomy"><Tags items={functionalCategoryLabels(entry.categories)} /></div></section> : null}
      <details className="plugin-technical-panel">
        <summary>开发者信息</summary>
        <div className="plugin-technical-grid">
          <div><span>稳定 ID</span><code>{entry.id}</code></div>
          <div><span>类型</span><strong>{marketKindLabels[entry.kind]}</strong></div>
          <div><span>当前版本</span><strong>v{entry.version}</strong></div>
          <div><span>来源</span><strong>{entry.sourceName}</strong></div>
          <div><span>来源类型</span><strong>{entry.sourceLabel}</strong></div>
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

function MarketPlanDialog({ pluginPlan, skillPlan, error, busy, installTargets, onClose, onPickLocation, onInstallPlugin, onInstallSkill }: {
  pluginPlan: PluginInstallPlan | null;
  skillPlan: SkillInstallPlan | null;
  error: string;
  busy: boolean;
  installTargets: { id: string; name: string; detected: boolean }[];
  onClose: () => void;
  onInstallPlugin: (item: PluginCatalogItem) => void;
  onInstallSkill: (item: OrganizationSkillCatalogItem, optionalPluginIds: string[], clients: string[], location: string) => void;
  onPickLocation: () => Promise<string>;
}) {
  const [optionalIds, setOptionalIds] = useState<string[]>([]);
  // 技能安装时可选投放目标；插件没有这个概念（它装在本机插件目录里）。
  const [targets, setTargets] = useState<string[]>(installTargets.map(client => client.id));
  const [targetsTouched, setTargetsTouched] = useState(false);
  // 安装位置：默认全局，可临时选一个目录，只对这次安装生效。
  const [location, setLocation] = useState("");
  useEffect(() => {
    if (targetsTouched) return;
    setTargets(installTargets.map(client => client.id));
  }, [installTargets, targetsTouched]);
  const targetSummary = targets.length === installTargets.length ? `投放到全部 ${installTargets.length} 个工具` : targets.length ? `投放到 ${targets.length} 个工具` : '不投放，只加入技能库';
  const locationSummary = location ? location : '全局（各 AI 工具的用户目录）';
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
        {skillPlan ? <div className="skill-plan-targets"><div className="skill-plan-targets-head"><strong>安装位置</strong><small>{locationSummary}</small></div>
          <div className="skill-plan-location"><input readOnly value={location} placeholder="默认安装到全局技能目录" /><button type="button" className="btn" disabled={busy} onClick={async () => { try { const picked = await onPickLocation(); if (picked) setLocation(picked); } catch { /* 取消不改动 */ } }}>选择目录…</button>{location ? <button type="button" className="text-action" disabled={busy} onClick={() => setLocation("")}>恢复全局</button> : null}</div>
        </div> : null}
        {skillPlan ? <div className="skill-plan-targets"><div className="skill-plan-targets-head"><strong>投放目标</strong><small>{targetSummary}</small></div>
          <div className="skill-plan-target-list">{installTargets.map(client => <label key={client.id}><input type="checkbox" checked={targets.includes(client.id)} disabled={busy} onChange={event => { setTargetsTouched(true); setTargets(current => event.target.checked ? [...current, client.id] : current.filter(id => id !== client.id)); }} /><span>{client.name}{client.detected ? '' : '（本机未检测到）'}</span></label>)}</div>
          <div className="skill-plan-targets-actions"><button type="button" className="text-action" disabled={busy} onClick={() => { setTargetsTouched(true); setTargets(installTargets.map(client => client.id)); }}>全选</button><button type="button" className="text-action" disabled={busy} onClick={() => { setTargetsTouched(true); setTargets([]); }}>都不投放</button></div>
        </div> : null}
        <div className="skill-dialog-actions">
          <button className="btn" onClick={onClose}>取消</button>
          <button className="btn btn-primary" disabled={!ready || busy} onClick={() => { if (skillPlan) onInstallSkill(skillPlan.skill, optionalIds, targets, location); else if (pluginPlan) onInstallPlugin(pluginPlan.plugin); }}><Download size={15} />确认安装</button>
        </div>
      </div>
    </div>
  );
}

function planActionDescription(action: string) {
  return ({ satisfied: '已安装', install: '将一并安装', update: '将一并更新', blocked: '被组织策略阻止', unavailable: '当前不可用' } as Record<string, string>)[action] || '需要处理';
}
