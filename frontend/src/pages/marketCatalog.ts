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
    minAgentVersion: entry.minAgentVersion,
    artifactId: entry.artifactId,
    sha256: entry.sha256,
  };
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
    const policyEntry = group.find(entry => entry.blocked) || group.find(entry => entry.managed) || primary;
    const installedVersion = group.find(entry => entry.installedVersion)?.installedVersion || '';
    const updateVersion = installedVersion
      ? candidates.map(candidate => candidate.version).filter(version => compareSemanticVersions(version, installedVersion) > 0).sort(compareSemanticVersions).pop() || ''
      : '';
    return {
      ...primary,
      key: primary.kind + ':' + primary.id,
      sourceLabel: sourceGroups.length > 1 ? '多来源' : primary.sourceLabel,
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
