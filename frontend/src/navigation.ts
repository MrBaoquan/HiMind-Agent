import { CalendarClock, Hammer, Home, Inbox, Library, ListTodo, MessageCircle, Store, Workflow, type LucideIcon } from 'lucide-react';
import type { PageKey } from './types';

/**
 * Agent UI 的导航唯一事实源。
 *
 * 侧栏和顶部“查看”菜单消费同一份模型，避免每个壳组件各自维护一套页面
 * 清单；pageLabel 提供不在菜单里的页面标题（例如设置窗口里的诊断页）。
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
  /** 侧栏和“查看”菜单中的可选分组标题；直接入口可以不显示标题。 */
  label?: string;
  items: NavigationItem[];
};

export const navigationSections: NavigationSection[] = [
  {
    id: 'ai',
    items: [
      { key: 'builtin-ai', icon: MessageCircle, label: 'AI 对话' },
    ],
  },
  {
    id: 'work',
    label: '我的工作',
    items: [
      { key: 'dashboard', icon: Home, label: '首页' },
      { key: 'tasks', icon: ListTodo, label: '我的任务' },
      { key: 'inbox', icon: Inbox, label: '待处理', badgeKey: 'inbox' },
    ],
  },
  {
    id: 'automation',
    label: '自动化工作流',
    items: [
      { key: 'workflows', icon: Workflow, label: '工作流' },
      { key: 'schedules', icon: CalendarClock, label: '定时计划' },
    ],
  },
  {
    id: 'capability',
    label: '能力拓展',
    items: [
      { key: 'extensions', icon: Store, label: '市场' },
      { key: 'installed', icon: Library, label: '我的能力' },
    ],
  },
  {
    id: 'developer',
    label: '扩展开发',
    items: [
      { key: 'development', icon: Hammer, label: '开发工作区' },
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
      'builtin-ai': 'AI 对话',
      approvals: '审批',
      tasks: '我的任务',
      ai: 'AI 连接',
      settings: '设置',
      logs: '日志与诊断',
      schedules: '定时计划',
      workflows: '工作流',
      extensions: '市场',
      installed: '我的能力',
      development: '扩展开发',
    } as Partial<Record<PageKey, string>>)[page]
    || page;
}
