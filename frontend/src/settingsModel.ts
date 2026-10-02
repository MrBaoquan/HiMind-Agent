import { Cable, Database, KeyRound, PlugZap, Power, ShieldAlert, ShieldCheck, Wrench, type LucideIcon } from 'lucide-react';

/** Surfaces inside the dedicated management window. */
export type SettingsWindowPanel = 'settings' | 'ai';

/**
 * 左栏条目。收敛过一次：把「同一件事的几段说明」合并成条目里的页签，
 * 而不是让每段说明各占一行导航。
 */
export type SettingsSection = 'accounts' | 'services' | 'automation' | 'approval' | 'general' | 'tooling' | 'diagnostics';

/** 页内页签：一个条目里并列的几个板块，互相之间没有先后关系。 */
export type SettingsTab = 'connectors' | 'remote-clients' | 'tools' | 'skills' | 'backup' | 'logs';

/** Rail key: a settings section, or the standalone AI-connections surface. */
export type SettingsRailKey = SettingsSection | 'ai';

type SettingsRailItem = {
  key: SettingsRailKey;
  label: string;
  /** One sentence shown under the section title in the content pane. */
  description: string;
  /** 检索别名：条目收敛后，用户仍会按原来的词找入口（SVN、Unity、日志……）。 */
  keywords: string[];
  icon: LucideIcon;
  panel: SettingsWindowPanel;
};

/**
 * The management window keeps a single source of truth for its navigation.
 * Mainstream desktop tools (ChatGPT, VS Code, Slack) expose one vertical rail
 * for every low-frequency surface instead of stacking top tabs over a second
 * navigation column, so the rail and the content header both read from here.
 */
export const SETTINGS_RAIL_GROUPS: Array<{ label: string; items: SettingsRailItem[] }> = [
  {
    label: '账号与连接',
    items: [
      { key: 'accounts', label: '账号', description: '管理 HiMind 账号、内网账号与代码仓库凭据。', keywords: ['himind', '内网', 'svn', 'git', 'github', '凭据', '登录', '授权'], icon: KeyRound, panel: 'settings' },
      { key: 'ai', label: 'AI 连接', description: '接入其他 AI 工具，管理模型服务与运行环境。', keywords: ['ai', 'mcp', '模型', '服务', '运行环境', '客户端', '注册', 'acp'], icon: Cable, panel: 'ai' },
      { key: 'services', label: '本机服务', description: '管理本机工作流连接器与远程控制客户端。', keywords: ['连接器', '远程控制', '客户端', '本机服务', 'codex', 'copilot'], icon: PlugZap, panel: 'settings' },
    ],
  },
  {
    label: '自动化与安全',
    items: [
      { key: 'automation', label: '自动化', description: '设置本机是否接收工作台任务、访问范围与执行工具。', keywords: ['远程任务', '工作台', '访问范围', '执行工具', '自动化'], icon: ShieldCheck, panel: 'settings' },
      { key: 'approval', label: '审批策略', description: '设置哪些操作需要确认，以及提醒方式与超时时间。', keywords: ['审批', '确认', '提醒', '超时', '安全', '放行'], icon: ShieldAlert, panel: 'settings' },
    ],
  },
  {
    label: '应用与数据',
    items: [
      { key: 'general', label: '通用', description: '配置软件更新和开机启动。', keywords: ['更新', '版本', '开机启动', '自启', '启动'], icon: Power, panel: 'settings' },
      { key: 'tooling', label: '本机工具与技能', description: '配置本机开发工具路径，以及技能的写入方式。', keywords: ['工具', '路径', 'unity', '技能', '目录', '写入', '安装'], icon: Wrench, panel: 'settings' },
      { key: 'diagnostics', label: '数据与诊断', description: '导出备份包与诊断信息，查看运行日志。', keywords: ['备份', '恢复', '日志', '诊断', '导出', '排查'], icon: Database, panel: 'settings' },
    ],
  },
];

const RAIL_ITEMS = SETTINGS_RAIL_GROUPS.flatMap(group => group.items);

/** 只有需要页签的条目才出现在这张表里；页签顺序即展示顺序，第一个就是默认落点。 */
const SETTINGS_SECTION_TABS: Partial<Record<SettingsSection, Array<{ key: SettingsTab; label: string }>>> = {
  services: [
    { key: 'connectors', label: '工作流连接器' },
    { key: 'remote-clients', label: '远程控制' },
  ],
  tooling: [
    { key: 'tools', label: '开发工具' },
    { key: 'skills', label: '技能写入方式' },
  ],
  diagnostics: [
    { key: 'backup', label: '备份与恢复' },
    { key: 'logs', label: '运行日志' },
  ],
};

/**
 * 旧深链与旧入口的落点。收敛导航时这些键位从界面上消失了，
 * 但历史链接、托盘入口和插件里写死的跳转还在用，必须继续落到对的位置。
 */
const LEGACY_SETTINGS_SECTIONS: Record<string, { section: SettingsSection; tab?: SettingsTab }> = {
  remote: { section: 'automation' },
  connectors: { section: 'services', tab: 'connectors' },
  'remote-tools': { section: 'services', tab: 'remote-clients' },
  tools: { section: 'tooling', tab: 'tools' },
  skills: { section: 'tooling', tab: 'skills' },
  backup: { section: 'diagnostics', tab: 'backup' },
  logs: { section: 'diagnostics', tab: 'logs' },
};

export const SETTINGS_SECTIONS: SettingsSection[] = ['accounts', 'services', 'automation', 'approval', 'general', 'tooling', 'diagnostics'];

export const SETTINGS_TABS: SettingsTab[] = ['connectors', 'remote-clients', 'tools', 'skills', 'backup', 'logs'];

export function isSettingsSection(value: string): value is SettingsSection {
  return (SETTINGS_SECTIONS as string[]).includes(value);
}

export function isSettingsTab(value: string): value is SettingsTab {
  return (SETTINGS_TABS as string[]).includes(value);
}

export function settingsSectionTabs(section: SettingsSection): Array<{ key: SettingsTab; label: string }> {
  return SETTINGS_SECTION_TABS[section] || [];
}

/** 条目没有页签时返回 null，页面直接渲染唯一那块内容。 */
export function settingsSectionTabFor(section: SettingsSection, requested?: SettingsTab | string | null): SettingsTab | null {
  const tabs = settingsSectionTabs(section);
  if (!tabs.length) return null;
  return requested && isSettingsTab(requested) && tabs.some(tab => tab.key === requested) ? requested : tabs[0].key;
}

export function settingsRailNavigation(key: SettingsRailKey): { panel: SettingsWindowPanel; section: SettingsSection | null; tab: SettingsTab | null } {
  if (key === 'ai') return { panel: 'ai', section: null, tab: null };
  return { panel: 'settings', section: key, tab: settingsSectionTabFor(key) };
}

export function settingsRailKey(panel: SettingsWindowPanel, section: SettingsSection): SettingsRailKey {
  if (panel === 'ai') return 'ai';
  return section;
}

export function settingsSectionMeta(section: SettingsSection): SettingsRailItem {
  return RAIL_ITEMS.find(item => item.key === section) || RAIL_ITEMS[0];
}

export function isSettingsRailKey(value: string): value is SettingsRailKey {
  return RAIL_ITEMS.some(item => item.key === value);
}

/**
 * 深链解析。URL 参数、初始化脚本和后端下发的事件都从这里过一遍，
 * 让「未知键回落」和「旧键改名」只有一套规则。
 */
export function settingsRoute(raw: { panel?: string | null; section?: string | null; tab?: string | null }): {
  panel: SettingsWindowPanel;
  section: SettingsSection;
  tab: SettingsTab | null;
  railKey: SettingsRailKey;
} {
  const panelRaw = (raw.panel || '').trim();
  const sectionRaw = (raw.section || '').trim();
  const tabRaw = (raw.tab || '').trim();
  // 「运行日志」曾经是独立面板：旧写法仍然落到数据与诊断的日志页签。
  const legacyLogs = panelRaw === 'logs';
  const legacy = LEGACY_SETTINGS_SECTIONS[sectionRaw];
  const panel: SettingsWindowPanel = panelRaw === 'ai' || sectionRaw === 'ai' ? 'ai' : 'settings';
  const section: SettingsSection = legacyLogs ? 'diagnostics' : isSettingsSection(sectionRaw) ? sectionRaw : legacy?.section || 'general';
  const tab = settingsSectionTabFor(section, legacyLogs ? 'logs' : isSettingsTab(tabRaw) ? tabRaw : legacy?.tab);
  return { panel, section, tab, railKey: panel === 'ai' ? 'ai' : section };
}

export function settingsRailItemMatches(item: SettingsRailItem, query: string): boolean {
  return settingsRailItemMatch(item, query).matched;
}

/**
 * 条目只靠别名命中时，标题里没有可高亮的字，用户会以为检索没生效。
 * 这里把命中的那个别名带出来，左栏用它补一行来源说明。
 */
export function settingsRailItemMatch(item: SettingsRailItem, query: string): { matched: boolean; alias: string | null } {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return { matched: true, alias: null };
  if (item.label.toLocaleLowerCase().includes(needle)) return { matched: true, alias: null };
  const alias = item.keywords.find(text => text.toLocaleLowerCase().includes(needle)) ?? null;
  return { matched: alias !== null, alias };
}

/** 命中部分要高亮，但只在标题里命中时才画；命中别名时整条照常显示即可。 */
export function splitSettingsRailLabel(label: string, query: string): Array<{ text: string; hit: boolean }> {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return [{ text: label, hit: false }];
  const index = label.toLocaleLowerCase().indexOf(needle);
  if (index < 0) return [{ text: label, hit: false }];
  const parts: Array<{ text: string; hit: boolean }> = [];
  if (index > 0) parts.push({ text: label.slice(0, index), hit: false });
  parts.push({ text: label.slice(index, index + needle.length), hit: true });
  if (index + needle.length < label.length) parts.push({ text: label.slice(index + needle.length), hit: false });
  return parts;
}
