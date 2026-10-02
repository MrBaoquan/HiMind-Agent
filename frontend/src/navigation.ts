import { CalendarClock, Hammer, Inbox, LayoutDashboard, Library, Store, Workflow, type LucideIcon } from 'lucide-react';
import type { PageKey } from './types';

/**
 * Agent UI 的导航唯一事实源。
 *
 * 侧栏和顶部“查看”菜单消费同一份模型，避免每个壳组件各自维护一套页面
 * 清单；pageLabel 提供不在菜单里的页面标题（例如活动）。
 * badgeKey 只表达领域状态的来源，不在这里读取状态。
 */
export type NavigationBadgeKey = 'inbox';

export type NavigationVisibilityContext = {
  dashboardEnabled: boolean;
};

export type NavigationItem = {
  key: PageKey;
  label: string;
  icon: LucideIcon;
  badgeKey?: NavigationBadgeKey;
  visibleIf?: (context: NavigationVisibilityContext) => boolean;
};

export type NavigationSection = {
  id: string;
  label: string;
  items: NavigationItem[];
};

export const navigationSections: NavigationSection[] = [
  {
    id: 'work',
    label: '我的工作',
    items: [
      { key: 'dashboard', icon: LayoutDashboard, label: 'Agent 状态' },
      { key: 'inbox', icon: Inbox, label: '待处理', badgeKey: 'inbox' },
    ],
  },
  {
    // 「市场」负责获得能力，「我的能力」负责拥有能力：装与管是两个面，
    // 插件 / 技能 / 工作流是页内页签，不再各占一行侧栏。
    id: 'capability',
    label: '能力',
    items: [
      { key: 'extensions', icon: Store, label: '市场' },
      { key: 'installed', icon: Library, label: '我的能力' },
    ],
  },
  {
    id: 'automation',
    label: '自动化',
    items: [
      { key: 'workflows', icon: Workflow, label: '工作流' },
      { key: 'schedules', icon: CalendarClock, label: '定时计划' },
    ],
  },
  {
    id: 'developer',
    label: '开发者',
    items: [
      { key: 'development', icon: Hammer, label: '扩展开发' },
    ],
  },
];

export const navigationItems = navigationSections.flatMap(section => section.items);

export function visibleNavigationSections(context: NavigationVisibilityContext) {
  return navigationSections
    .map(section => ({ ...section, items: section.items.filter(item => !item.visibleIf || item.visibleIf(context)) }))
    .filter(section => section.items.length > 0);
}

export function navigationSectionForPage(page: PageKey) {
  return navigationSections.find(section => section.items.some(item => item.key === page));
}

export function pageLabel(page: PageKey) {
  return navigationItems.find(item => item.key === page)?.label
    || ({
      'builtin-ai': 'HiMind AI',
      approvals: '审批',
      tasks: '活动',
      ai: 'AI 连接',
      settings: '设置',
      logs: '运行日志',
    } as Partial<Record<PageKey, string>>)[page]
    || page;
}
