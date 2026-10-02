/**
 * 市场列表的纯数据层。
 *
 * 目录（Catalog）按来源返回，市场按产品呈现：同一个稳定 ID 可能同时来自本地
 * 源码、GitHub Release 和组织发布。这里把来源形状收敛成产品形状，同时保留
 * 安装时必须绑定哪一条来源的信息，供页面选择版本时使用。
 * 抽成纯模块是为了让这段语义能脱离 React 被自检脚本覆盖。
 */

export type MarketSourceGroup = 'system' | 'organization' | 'local' | 'remote';

export type MarketDependency = {
  id: string;
  name: string;
  required: boolean;
  hint: string;
};

/** 一个可安装候选：同一产品在某个来源上的某个版本。 */
export type MarketCandidate = {
  version: string;
  publishedAt: string;
  notes: string;
  source: string;
  sourceLabel: string;
  /// 用户能认出来的来源名；类型词只留给开发者信息。
  sourceName?: string;
  minAgentVersion: string;
  artifactId: string;
  sha256: string;
};

/** 市场条目中与来源聚合相关的字段。 */
export type MarketProduct = {
  key: string;
  kind: string;
  id: string;
  version: string;
  source: string;
  sourceGroup: MarketSourceGroup;
  sourceLabel: string;
  /// 配置过的扩展源会有 ID；据此换成配置里写的名字。
  sourceId?: string;
  sourceName?: string;
  sourceKeys?: string[];
  sourceNames?: string[];
  sourceGroups?: MarketSourceGroup[];
  sourceCandidates?: MarketCandidate[];
  artifactId: string;
  sha256: string;
  minAgentVersion: string;
  categories: string[];
  capabilityIds: string[];
  dependencies: MarketDependency[];
  support: string[];
  policyLabel: string;
  policyKind: 'success' | 'warn' | 'danger' | 'neutral';
  blocked: boolean;
  managed: boolean;
  installedVersion: string;
  updateVersion: string;
};

export function compareSemanticVersions(left: string, right: string) {
  const parse = (value: string) => value.split(/[.+-]/).slice(0, 3).map(part => Number.parseInt(part, 10) || 0);
  const leftParts = parse(left);
  const rightParts = parse(right);
  for (let index = 0; index < 3; index += 1) {
    if ((leftParts[index] || 0) !== (rightParts[index] || 0)) return (leftParts[index] || 0) - (rightParts[index] || 0);
  }
  return 0;
}

export function newerVersion(available: string, installed: string) {
  if (!available || !installed) return '';
  return compareSemanticVersions(available, installed) > 0 ? available : '';
}

/** 一个分发单元的制品视角，只保留判断「能不能更新」需要的字段。 */
export type UnitUpdateAsset = { asset_kind: string; asset_id: string; version: string };

/**
 * 「可更新」的判定只能有一个事实源，就是市场条目本身（`MarketProduct.updateVersion`，
 * 已经在市场层排除组织管理与组织禁止的制品）。来源卡片拿到的是市场算好的
 * `kind:id → 更新目标版本`，只负责回答「这条更新是不是我这个来源提供的」：
 * 制品版本恰好等于市场的更新目标，才算这个来源的待更新项。
 *
 * 之前这里用「制品版本 > 本机已装版本」自己再算一遍，结果是同一台机器上出现
 * 「市场说 0 项可更新」而「来源卡片说 4 项待更新」——因为两处读的已装版本
 * 不是同一个来源（市场读台账，卡片读渲染目录）。同一条规则只留一份，才能避免
 * 再次分叉；同版本不同摘要属于「重新安装」，本来也不该报成待更新。
 */
export function countUnitUpdates(
  assets: UnitUpdateAsset[],
  updateTargets: Map<string, string>,
) {
  let updates = 0;
  for (const asset of assets) {
    const target = updateTargets.get(`${asset.asset_kind}:${asset.asset_id}`);
    if (target && target === asset.version) updates += 1;
  }
  return updates;
}

/**
 * 版本比较只有这一份实现：来源管理判断「这次安装是不是降级」、扩展开发排序草稿
 * 版本都走它。之前两处各写一遍，规则一旦分叉就会出现「一处说降级、一处说升级」。
 * 返回正数表示 left 高于 right。
 */
export function compareVersions(left: string, right: string) {
  const a = left.split(/[.+-]/).map(value => Number.parseInt(value, 10) || 0);
  const b = right.split(/[.+-]/).map(value => Number.parseInt(value, 10) || 0);
  for (let index = 0; index < Math.max(a.length, b.length); index += 1) {
    const diff = (a[index] || 0) - (b[index] || 0);
    if (diff) return diff;
  }
  return left.localeCompare(right);
}

/// Extension sources carry acquisition prefixes on catalog items and channel
/// names on distribution items, so the group is resolved from both.
export function resolveSource(raw?: string, channel?: string): { group: MarketSourceGroup; label: string } {
  const value = (raw || '').trim().toLowerCase();
  const channelValue = (channel || '').trim().toLowerCase();
  if (value.startsWith('local') || channelValue === 'local') return { group: 'local', label: '本地源码' };
  if (value.startsWith('github') || value.startsWith('remote') || channelValue === 'remote') return { group: 'remote', label: 'GitHub 发布' };
  if (value === 'system' || channelValue === 'system') return { group: 'system', label: '系统内置' };
  return { group: 'organization', label: '组织发布' };
}

/**
 * 目录项的来源带的是配置 ID（local:local-1a2b / github:github-3c4d），
 * 用它可以反查用户在「来源管理」里给这个源起的名字。没有配置 ID 的来源
 * （系统内置、工作台下发、随手导入的本机目录）只能按类型兜底。
 */
export function resolveSourceIdentity(raw?: string, channel?: string): { group: MarketSourceGroup; label: string; id: string } {
  const value = (raw || '').trim();
  const lower = value.toLowerCase();
  const resolved = resolveSource(value, channel);
  for (const prefix of ['local:', 'github:', 'remote:']) {
    if (lower.startsWith(prefix)) return { ...resolved, id: value.slice(prefix.length).trim() };
  }
  return { ...resolved, id: '' };
}

/// 没有配置项可查的来源，用产品自己的叫法，而不是"本地源码/组织发布"这类类型词。
export function sourceNameFor(group: MarketSourceGroup) {
  return ({ system: '系统内置', organization: 'AI 工作台', local: '本机开发目录', remote: 'GitHub 导入' } as Record<MarketSourceGroup, string>)[group];
}

/// 目录名或仓库名：本地来源没起名字时，后端会把绝对路径写进 name，
/// 直接显示就把本机路径带到了市场列表里，所以统一只取最后一段。
export function sourceBaseName(value: string) {
  const trimmed = (value || '').replace(/[\\/]+$/, '');
  return trimmed.split(/[\\/]/).pop() || trimmed;
}

/// 用户能认出来的来源名：路径类名字退回目录名/仓库名，其余原样使用。
export function friendlySourceName(value: string, fallback?: string) {
  const trimmed = (value || '').trim();
  if (!trimmed) return fallback ? sourceBaseName(fallback) : '';
  const normalized = trimmed.replace(/[\\/]+$/, '').toLowerCase();
  if (fallback && normalized === fallback.replace(/[\\/]+$/, '').toLowerCase()) return sourceBaseName(fallback);
  if (/^[a-z]:[\\/]/i.test(trimmed) || trimmed.includes('\\\\')) return sourceBaseName(trimmed);
  return trimmed;
}

/// 来源筛选用的稳定键：配置过的源用它的 ID，其余按类型分组。
export function sourceKeyOf(identity: { group: MarketSourceGroup; id: string }) {
  return identity.id || `group:${identity.group}`;
}

export function entryIdentity(kind: string, id: string, source: string, artifactId: string, sha256: string) {
  return [kind, id, source || 'unknown', artifactId || sha256 || 'mutable'].join(':');
}

export function versionIdentity(candidate: MarketCandidate) {
  return [candidate.source || 'unknown', candidate.version, candidate.artifactId || candidate.sha256 || 'mutable'].join(':');
}

export function sourceDisplayLabel(source: string, fallback = '组织发布') {
  if (!source) return fallback;
  return resolveSource(source).label;
}

export function marketVersionFromEntry(entry: MarketProduct): MarketCandidate {
  return {
    version: entry.version,
    publishedAt: '',
    notes: '',
    source: entry.source,
    sourceLabel: sourceDisplayLabel(entry.source, entry.sourceLabel),
    sourceName: entry.sourceName || sourceDisplayLabel(entry.source, entry.sourceLabel),
    minAgentVersion: entry.minAgentVersion,
    artifactId: entry.artifactId,
    sha256: entry.sha256,
  };
}

/**
 * 安装类动作的动词只在这里定义一次：安装 / 更新到 / 重新安装 / 降级到。
 * 同一个动作在插件页、技能页、市场详情里必须叫同一个名字，并且带上目标版本号，
 * 否则用户没法判断"安装此版本"到底会装成哪一个。
 */
export function installActionLabel(options: { target: string; installed?: string; locked?: string }) {
  const { target, installed, locked } = options;
  if (locked) return locked;
  if (!target) return '安装';
  if (!installed) return `安装 v${target}`;
  const diff = compareSemanticVersions(target, installed);
  if (diff > 0) return `更新到 v${target}`;
  if (diff === 0) return `重新安装 v${target}`;
  return `降级到 v${target}`;
}

/// 同一产品来自多个来源时，用谁的身份作为列表主视图：组织与系统优先级最高，
/// 本地源码最低，避免开发中的本地预览盖住组织策略。
function sourcePriority(group: MarketSourceGroup) {
  return ({ system: 0, organization: 1, remote: 2, local: 3 } as Record<MarketSourceGroup, number>)[group];
}

/**
 * Catalogs are source-shaped, but the market is product-shaped. Merge the
 * same stable ID across Local/GitHub/Dashboard so users see one product and
 * choose the source only when selecting a version to install.
 */
export function mergeMarketEntries<T extends MarketProduct>(entries: T[]): T[] {
  const grouped = new Map<string, T[]>();
  for (const entry of entries) {
    const key = entry.kind + ':' + entry.id;
    grouped.set(key, [...(grouped.get(key) || []), entry]);
  }
  return [...grouped.values()].map(group => {
    const primary = [...group].sort((left, right) => sourcePriority(left.sourceGroup) - sourcePriority(right.sourceGroup))[0];
    const candidates = [...new Map(group.flatMap(entry => (entry.sourceCandidates || [marketVersionFromEntry(entry)]).map(candidate => [versionIdentity(candidate), candidate] as const))).values()];
    const sourceGroups = [...new Set(group.flatMap(entry => entry.sourceGroups || [entry.sourceGroup]))];
    // 来源筛选要按真实来源名走，所以这里保留「键 → 名字」的有序对，而不是只剩类型。
    const sourcePairs = [...new Map([...group]
      .sort((left, right) => sourcePriority(left.sourceGroup) - sourcePriority(right.sourceGroup))
      .map(entry => [entry.sourceId || `group:${entry.sourceGroup}`, entry.sourceName || entry.sourceLabel] as const)).entries()];
    const policyEntry = group.find(entry => entry.blocked) || group.find(entry => entry.managed) || primary;
    const installedVersion = group.find(entry => entry.installedVersion)?.installedVersion || '';
    const updateVersion = installedVersion
      ? candidates.map(candidate => candidate.version).filter(version => compareSemanticVersions(version, installedVersion) > 0).sort(compareSemanticVersions).pop() || ''
      : '';
    // 头部版本和详情页主按钮绑定的是同一个候选：本机装了就给本机那一版，有更高版本就给
    // 更新目标，都没装才给目录里最新的一版。主来源优先级只决定身份与策略，不决定版本——
    // 组织目录停在新版本之后时，按主来源取版本会让已装新版的用户看到「降级」。
    const newest = [...candidates].sort((left, right) => compareSemanticVersions(right.version, left.version))[0];
    const updateCandidate = installedVersion && newest && compareSemanticVersions(newest.version, installedVersion) > 0 ? newest : null;
    const installedCandidate = installedVersion ? candidates.find(candidate => candidate.version === installedVersion) : undefined;
    const headline = updateCandidate || installedCandidate || newest || marketVersionFromEntry(primary);
    return {
      ...primary,
      key: primary.kind + ':' + primary.id,
      // 版本、来源和制品必须整体换成同一个候选，只换版本号会把某个来源的版本号
      // 和另一个来源的制品摘要拼在一起，安装时会去取一份对不上的包。
      version: headline.version,
      source: headline.source,
      artifactId: headline.artifactId,
      sha256: headline.sha256,
      sourceLabel: sourceGroups.length > 1 ? '多来源' : primary.sourceLabel,
      sourceName: sourcePairs.length > 1 ? '多来源' : sourcePairs[0]?.[1] || primary.sourceName || primary.sourceLabel,
      sourceKeys: sourcePairs.map(([key]) => key),
      sourceNames: sourcePairs.map(([, name]) => name),
      sourceGroups,
      sourceCandidates: candidates,
      categories: [...new Set(group.flatMap(entry => entry.categories))],
      capabilityIds: [...new Set(group.flatMap(entry => entry.capabilityIds))],
      dependencies: [...new Map(group.flatMap(entry => entry.dependencies).map(dependency => [dependency.id, dependency])).values()],
      support: [...new Set(group.flatMap(entry => entry.support))],
      blocked: group.some(entry => entry.blocked),
      managed: group.some(entry => entry.managed),
      policyLabel: policyEntry.policyLabel,
      policyKind: group.some(entry => entry.blocked) ? 'danger' : policyEntry.policyKind,
      installedVersion,
      updateVersion,
    };
  });
}
