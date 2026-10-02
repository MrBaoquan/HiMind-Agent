// 工具目录（MCP catalog）面板的纯逻辑：信任文案、来源摘要、搜索过滤、
// 安装前的前置检查。刻意不依赖 tauri / react，方便 check-mcp-servers.mts 直接跑断言。

/** 来源的信任等级，和后端 CatalogTrust 一一对应。 */
export type CatalogTrust = 'curated' | 'verified' | 'unverified';

export type CatalogInput = {
  /** 提交时的键，后端按这个吃值：`env:API_KEY` / `header:Authorization` / `arg:directory`。 */
  key: string;
  label: string;
  kind: string;
  required: boolean;
  secret: boolean;
  description: string;
  placeholder: string;
  value: string;
  /** `directory` 表示这个值要用目录选择器，其余是文本框。 */
  picker: string;
};

export type CatalogEntry = {
  id: string;
  source_id: string;
  source_label: string;
  trust: CatalogTrust;
  title: string;
  description: string;
  version: string;
  schema: string;
  repository: string;
  website_url: string;
  transport: string;
  runtime: string;
  runtime_label: string;
  command_preview: string;
  installable: boolean;
  /** 装不了时给用户看的一句话，永远不是空字符串。 */
  reason: string;
  /** 已经装过就是服务 ID，没装过是空字符串。 */
  installed_as: string;
  suggested_name: string;
  inputs: CatalogInput[];
};

export type CatalogSource = {
  id: string;
  label: string;
  trust: CatalogTrust;
  url: string;
  count: number;
  fetched_at: number;
  acknowledged: boolean;
  error: string;
};

export type CatalogView = {
  entries: CatalogEntry[];
  sources: CatalogSource[];
  fetched_at: number;
  stale: boolean;
};

export const BUILTIN_SOURCE_ID = 'builtin';

/**
 * 第三方目录首屏最多铺这么多条。公开目录是海量数据（实测官方源一次能返回 500+ 条），
 * 一屏铺满只会让人无从下手；先给一屏，其余交给搜索和「显示更多」。
 */
export const CATALOG_PAGE_SIZE = 30;

/** 一次「显示更多」加铺的条数。 */
export const CATALOG_PAGE_STEP = 30;

const TRUST_LABEL: Record<CatalogTrust, string> = {
  curated: '精选',
  verified: '已签名',
  unverified: '未验证来源',
};

export function trustLabel(trust: CatalogTrust): string {
  return TRUST_LABEL[trust] ?? TRUST_LABEL.unverified;
}

export function trustPillKind(trust: CatalogTrust): 'success' | 'warn' | 'neutral' {
  if (trust === 'curated') return 'success';
  if (trust === 'verified') return 'neutral';
  return 'warn';
}

/** 未验证来源的工具装完默认不启用，界面必须把这件事写出来。 */
export function installsDisabled(trust: CatalogTrust): boolean {
  return trust === 'unverified';
}

/** 未验证来源第一次安装前要确认一次；确认记录按来源，不在每一条上重复问。 */
export function needsAcknowledgement(trust: CatalogTrust, acknowledged: boolean): boolean {
  return installsDisabled(trust) && !acknowledged;
}

export function filterEntries(entries: CatalogEntry[], query: string): CatalogEntry[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return entries;
  return entries.filter(entry =>
    [entry.title, entry.description, entry.id, entry.source_label, entry.runtime_label]
      .some(field => field.toLowerCase().includes(needle)));
}

/** 精选（随包分发的内置快照）单独置顶：这些是「打开就能用」的那几条。 */
export function isCuratedEntry(entry: CatalogEntry): boolean {
  return entry.source_id === BUILTIN_SOURCE_ID || entry.trust === 'curated';
}

export type CatalogGroup = {
  id: 'curated' | 'other';
  label: string;
  entries: CatalogEntry[];
};

/**
 * 目录分组：精选一组置顶，其余来源合成一组，第三方默认只铺 `limit` 条。
 * 搜索走同一条路——命中很多时也只铺一屏，剩下的数交给「显示更多」。
 */
export function groupCatalog(
  entries: CatalogEntry[],
  query: string,
  limit: number = CATALOG_PAGE_SIZE,
): { groups: CatalogGroup[]; matched: number; hidden: number } {
  const matched = filterEntries(entries, query);
  const curated = matched.filter(isCuratedEntry);
  const others = matched.filter(entry => !isCuratedEntry(entry));
  const groups: CatalogGroup[] = [];
  if (curated.length) groups.push({ id: 'curated', label: '精选工具', entries: curated });
  const visibleOthers = others.slice(0, Math.max(0, limit));
  if (visibleOthers.length) groups.push({ id: 'other', label: '第三方目录', entries: visibleOthers });
  return { groups, matched: matched.length, hidden: Math.max(0, others.length - Math.max(0, limit)) };
}

/**
 * 第三方目录那一行的风险提示。本地 MCP 是子进程，能力不受我们约束，
 * 这句按 VS Code 的口径写：说清后果，只提一次（组级），不贴到每一张卡上。
 */
export const CATALOG_RISK_NOTE = '本地 MCP 会在这台机器上执行任意代码，只装你信任的来源。';

export function relativeTime(fetchedAt: number, nowSeconds: number = Math.floor(Date.now() / 1000)): string {
  if (!fetchedAt) return '还没同步过';
  const delta = Math.max(0, nowSeconds - fetchedAt);
  if (delta < 60) return '刚刚同步';
  if (delta < 3600) return `${Math.floor(delta / 60)} 分钟前同步`;
  if (delta < 86400) return `${Math.floor(delta / 3600)} 小时前同步`;
  return `${Math.floor(delta / 86400)} 天前同步`;
}

/** 一行来源摘要：坏了说坏在哪，正常说有多少条、什么时候拉的。 */
export function sourceNote(source: CatalogSource): string {
  if (source.error) return source.error;
  if (source.id === BUILTIN_SOURCE_ID) return `${source.count} 个内置工具`;
  return `${source.count} 条 · ${relativeTime(source.fetched_at)}`;
}

export function networkSources(view: CatalogView): CatalogSource[] {
  return view.sources.filter(source => source.id !== BUILTIN_SOURCE_ID);
}

/** 悬浮时展开每个来源的明细，界面上只留一行，避免菜单里堆说明。 */
export function sourceBreakdown(view: CatalogView): string {
  if (!view.sources.length) return '还没读到目录来源';
  return view.sources.map(source => `${source.label}：${sourceNote(source)}`).join('\n');
}

/**
 * 目录那一行摘要。来源坏了就说坏在哪，别把「拉不到」显示成「没有」。
 *
 * 这里不报条数：市场页签数的是整个目录（含内置精选），第三方目录那一行数的是
 * 远端来源，同一屏上再摆第三个数（只有远端）只会让人对不上账——页签写 527、
 * 面板标题写 523 就是这么来的。条数各归其位，这里只说新不新鲜。
 */
export function catalogNote(view: CatalogView): string {
  const sources = networkSources(view);
  if (!sources.length) return '只看内置精选，尚未添加目录来源';
  const failed = sources.find(source => source.error);
  if (failed) return `${failed.label}：${failed.error}`;
  const total = sources.reduce((sum, source) => sum + source.count, 0);
  if (!total) return '目录还没同步，点刷新拉取';
  return view.stale ? `目录 ${relativeTime(view.fetched_at)}，建议刷新` : `目录 ${relativeTime(view.fetched_at)}`;
}

/**
 * 卡片上那一行依赖说明。远端条目没有本机依赖，就不摆这一行。
 * `missing` 只在本机确实查过、而且没装的时候才为真。
 */
export function runtimeNote(
  entry: CatalogEntry,
  requirements: Record<string, { available: boolean }>,
): { label: string; missing: boolean } | null {
  if (!entry.runtime) return null;
  const label = entry.runtime_label || entry.runtime;
  const requirement = requirements[entry.runtime];
  return { label: `需要 ${label}`, missing: Boolean(requirement) && !requirement.available };
}

/** 装完之后要说的那句话。未验证来源默认停用，必须写出来，不能只报成功。 */
export function installNotice(entry: CatalogEntry): string {
  if (installsDisabled(entry.trust)) {
    return `已添加「${entry.title}」，未验证来源默认停用；确认没问题后在「已添加」里启用。`;
  }
  return `已添加「${entry.title}」，HiMind AI 已重新连接。`;
}

/**
 * 装之前先在本地说清楚缺什么。后端仍然会拦一次，但让用户少跑一趟往返。
 * 返回空字符串表示可以安装。
 */
export function installBlocker(
  entry: CatalogEntry,
  values: Record<string, string>,
  checked: boolean,
  acknowledged: boolean,
  requirements: Record<string, { available: boolean }>,
): string {
  if (!entry.installable) return entry.reason || '这个工具当前装不了。';
  if (entry.installed_as) return `已经装过了，服务 ID 是「${entry.installed_as}」。`;
  const missing = entry.inputs.find(input => input.required && !(values[input.key] ?? '').trim());
  if (missing) return `请先填写「${missing.label}」。`;
  if (needsAcknowledgement(entry.trust, acknowledged) && !checked) return '请先确认已了解这条工具会在本机执行代码。';
  const requirement = entry.runtime ? requirements[entry.runtime] : undefined;
  if (requirement && !requirement.available) return `这条工具需要 ${entry.runtime_label || entry.runtime}，本机还没装。`;
  return '';
}

/** 安装表单打开时，把条目里写死的默认值先铺进去，用户只需要补必填项。 */
export function initialValues(entry: CatalogEntry): Record<string, string> {
  const values: Record<string, string> = {};
  for (const input of entry.inputs) {
    if (input.value) values[input.key] = input.value;
  }
  return values;
}

export function emptyCatalogView(): CatalogView {
  return { entries: [], sources: [], fetched_at: 0, stale: true };
}
