import { Blocks, BookOpen, Cable, CalendarClock, FileText, Hammer, LayoutDashboard, ListChecks, Settings, Store, Workflow, type LucideIcon } from 'lucide-react';
import type { PageKey } from './types';

/**
 * Agent UI 的导航唯一事实源。
 *
 * 侧栏、顶部“查看”菜单和页面标题都消费同一份模型，避免每个壳组件
 * 各自维护一套页面清单。badgeKey 只表达领域状态的来源，不在这里读取状态。
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
      { key: 'dashboard', icon: LayoutDashboard, label: '概览' },
      { key: 'inbox', icon: ListChecks, label: '待处理', badgeKey: 'inbox' },
      { key: 'workflows', icon: Workflow, label: '工作流' },
      { key: 'schedules', icon: CalendarClock, label: '定时任务' },
    ],
  },
  {
    id: 'capabilities',
    label: '扩展',
    items: [
      { key: 'extensions', icon: Store, label: '市场' },
      { key: 'plugins', icon: Blocks, label: '插件' },
      { key: 'skills', icon: BookOpen, label: '技能' },
    ],
  },
  {
    id: 'connections',
    label: '连接',
    items: [
      { key: 'ai', icon: Cable, label: 'AI 连接' },
    ],
  },
  {
    id: 'system',
    label: '系统',
    items: [
      { key: 'logs', icon: FileText, label: '运行日志' },
      { key: 'settings', icon: Settings, label: '设置' },
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

export function pageLabel(page: PageKey) {
  return navigationItems.find(item => item.key === page)?.label
    || ({ 'builtin-ai': 'HiMind AI', approvals: '审批' } as Partial<Record<PageKey, string>>)[page]
    || page;
}
