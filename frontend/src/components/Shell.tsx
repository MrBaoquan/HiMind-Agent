import { useEffect, useMemo, useRef, useState, type MouseEvent as ReactMouseEvent, type ReactNode, type RefObject } from 'react';
import { Activity, AppWindow, Blocks, BookOpen, Cable, CalendarDays, CheckCircle2, ChevronDown, CircleAlert, CircleUserRound, Clapperboard, Clock3, Database, Download, ExternalLink, FileCode2, FileText, FolderOpen, Info, LayoutDashboard, LayoutGrid, LogOut, MessageCircle, Minus, MonitorPlay, Music, Package, PanelLeftClose, PanelLeftOpen, Puzzle, Radar, RefreshCw, Search, Settings, Sparkles, Square, Star, Terminal, Video, Workflow, Wrench, X, type LucideIcon } from 'lucide-react';
// 形变吃的是图标数据，静态图标仍走 lucide-react 组件；两边的图标集版本必须一致，
// 否则同一个图标在「形变端点」和「静态渲染」下长得不一样（由 check:icon-version 守住）。
import { Activity as activityIconData, LoaderCircle as loaderCircleIconData } from 'lucide';
import { BusyIndicator } from './BusyIndicator';
import { MorphIcon } from './MorphIcon';
import { useModalFocus } from './useModalFocus';
import type { NavigationTarget, PageKey } from '../types';
import { pageLabel, visibleNavigationSections, type NavigationItem } from '../navigation';
import type { CurrentTaskStatus, DashboardIdentityStatus, PluginQuickAccessView } from '../services/agentApi';
import { compareQuickViews, dedupeQuickAccessViews, pickUniqueQuickAccessLabels, quickAccessKey, quickAccessLabel } from '../data/quickAccess';
import { shortRunId, taskElapsedSeconds, taskTypeLabel } from '../pages/taskView';
import { formatElapsedCn } from '../pages/workflowRunView';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';

/**
 * 常驻运行条的数据：只取「第一条在跑的运行」做代表，数量用来提示后面还有。
 * 工作台任务和本机运行共用同一条 UI，只是数据来源不同。
 */
export type ActiveRunSummary = {
  /** 形如「工作流 · 科技雷达日报」。 */
  title: string;
  runId: string;
  /** 当前步骤，或「等待你的处理」。 */
  stage: string;
  startedAt: string;
  /** 同时在跑的数量；大于 1 时条上会提示还有其他运行。 */
  count: number;
};

/** 运行条上的「已运行」必须自己走秒：只有确实有运行中的运行才挂定时器。 */
function useRunElapsed(startedAt: string | null | undefined): number | null {
  const [elapsed, setElapsed] = useState<number | null>(null);
  useEffect(() => {
    if (!startedAt) { setElapsed(null); return; }
    const tick = () => setElapsed(taskElapsedSeconds(startedAt, Date.now()));
    tick();
    const timer = window.setInterval(tick, 1000);
    return () => window.clearInterval(timer);
  }, [startedAt]);
  return elapsed;
}

type ShellProps = {
  currentPage: PageKey;
  approvalCount: number;
  workflowApprovalCount: number;
  identity: DashboardIdentityStatus | null;
  dashboardEnabled: boolean;
  agentVersion: string;
  updateBusy: boolean;
  currentTask: CurrentTaskStatus | null;
  /** 本机正在跑的工作流/技能运行数：为空表示没有活动运行。 */
  activeRunCount: number;
  /** 本机第一条活动运行的摘要：没有工作台任务时用它撑起常驻运行条。 */
  activeRun: ActiveRunSummary | null;
  quickPluginViews: PluginQuickAccessView[];
  onNavigate: (target: NavigationTarget) => void;
  onOpenPluginView: (pluginId: string, viewId: string) => void;
  onOpenSettings: () => void;
  onOpenDashboard: () => void;
  onOpenBuiltinAi: () => void;
  onCheckUpdate: () => void;
  onOpenAgentDirectory: () => void;
  onQuit: () => void;
  children: ReactNode;
};

const quickViewIconMap: Record<string, LucideIcon> = {
  'app-window': AppWindow,
  appwindow: AppWindow,
  app: AppWindow,
  window: AppWindow,
  blocks: Blocks,
  book: BookOpen,
  'book-open': BookOpen,
  cable: Cable,
  calendar: CalendarDays,
  'calendar-days': CalendarDays,
  clapperboard: Clapperboard,
  code: FileCode2,
  download: Download,
  database: Database,
  file: FileText,
  'file-code': FileCode2,
  'file-code-2': FileCode2,
  folder: FolderOpen,
  'folder-open': FolderOpen,
  grid: LayoutGrid,
  'layout-grid': LayoutGrid,
  dashboard: LayoutDashboard,
  chart: LayoutGrid,
  table: LayoutGrid,
  extension: Puzzle,
  plugin: Puzzle,
  film: Clapperboard,
  media: Video,
  music: Music,
  monitor: MonitorPlay,
  'monitor-play': MonitorPlay,
  package: Package,
  puzzle: Puzzle,
  radar: Radar,
  settings: Settings,
  sparkles: Sparkles,
  terminal: Terminal,
  video: Video,
  play: MonitorPlay,
  flow: Workflow,
  workflow: Workflow,
  wrench: Wrench,
};

function quickViewIcon(icon: string | undefined) {
  const key = (icon || '').trim().toLowerCase().replace(/[ _]+/g, '-');
  return quickViewIconMap[key] || AppWindow;
}

type QuickAccessUsage = Record<string, { count: number; lastUsed: number }>;
type QuickAccessFilter = 'all' | 'favorites' | 'frequent';

const QUICK_ACCESS_FAVORITES_KEY = 'himind.quick-access.favorites';
const QUICK_ACCESS_USAGE_KEY = 'himind.quick-access.usage';
const QUICK_ACCESS_VISIBLE_LIMIT = 4;

function readQuickAccessFavorites(): string[] {
  try {
    const value = JSON.parse(window.localStorage.getItem(QUICK_ACCESS_FAVORITES_KEY) || '[]');
    return Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string') : [];
  } catch {
    return [];
  }
}

function readQuickAccessUsage(): QuickAccessUsage {
  try {
    const value = JSON.parse(window.localStorage.getItem(QUICK_ACCESS_USAGE_KEY) || '{}');
    if (!value || typeof value !== 'object' || Array.isArray(value)) return {};
    const usage: QuickAccessUsage = {};
    for (const [key, raw] of Object.entries(value)) {
      if (!raw || typeof raw !== 'object' || Array.isArray(raw)) continue;
      const item = raw as { count?: unknown; lastUsed?: unknown };
      const count = Number(item.count);
      const lastUsed = Number(item.lastUsed);
      if (!Number.isFinite(count) || count <= 0) continue;
      usage[key] = { count: Math.min(Math.floor(count), 9999), lastUsed: Number.isFinite(lastUsed) ? lastUsed : 0 };
    }
    return usage;
  } catch {
    return {};
  }
}

function quickViewSearchText(view: PluginQuickAccessView) {
  return [view.short_title, view.title, view.plugin_name, view.plugin_id, view.view_id].join('\n').toLocaleLowerCase('zh-CN');
}

type QuickAccessDialogProps = {
  views: PluginQuickAccessView[];
  favorites: Set<string>;
  usage: QuickAccessUsage;
  onToggleFavorite: (view: PluginQuickAccessView) => void;
  onOpen: (view: PluginQuickAccessView) => void;
  onClose: () => void;
  returnFocusRef: RefObject<HTMLButtonElement | null>;
};

function QuickAccessDialog({ views, favorites, usage, onToggleFavorite, onOpen, onClose, returnFocusRef }: QuickAccessDialogProps) {
  const dialogRef = useRef<HTMLElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState<QuickAccessFilter>('all');
  useModalFocus(dialogRef, true, onClose, searchRef, returnFocusRef);

  const favoriteViews = useMemo(
    () => views.filter(view => favorites.has(quickAccessKey(view))).sort(compareQuickViews),
    [favorites, views],
  );
  const frequentViews = useMemo(
    () => views
      .filter(view => usage[quickAccessKey(view)]?.count > 0)
      .sort((left, right) => {
        const leftUsage = usage[quickAccessKey(left)];
        const rightUsage = usage[quickAccessKey(right)];
        return rightUsage.count - leftUsage.count
          || rightUsage.lastUsed - leftUsage.lastUsed
          || compareQuickViews(left, right);
      }),
    [usage, views],
  );
  const sourceViews = filter === 'favorites' ? favoriteViews : filter === 'frequent' ? frequentViews : [...views].sort(compareQuickViews);
  const normalizedQuery = query.trim().toLocaleLowerCase('zh-CN');
  const visibleViews = normalizedQuery
    ? sourceViews.filter(view => quickViewSearchText(view).includes(normalizedQuery))
    : sourceViews;

  return (
    <div className="quick-access-dialog-backdrop" role="presentation" onMouseDown={event => { if (event.target === event.currentTarget) onClose(); }}>
      <section ref={dialogRef} className="quick-access-dialog" role="dialog" aria-modal="true" aria-labelledby="quick-access-dialog-title" tabIndex={-1}>
        <header className="quick-access-dialog-head">
          <div>
            <h2 id="quick-access-dialog-title">全部工具</h2>
            <p>搜索插件入口，收藏常用的工具。</p>
          </div>
          <button type="button" className="btn btn-icon" aria-label="关闭工具面板" title="关闭" onClick={onClose}><X size={16} /></button>
        </header>
        <div className="quick-access-dialog-search" role="search">
          <Search size={16} aria-hidden="true" />
          <input
            ref={searchRef}
            type="search"
            value={query}
            onChange={event => setQuery(event.target.value)}
            onKeyDown={event => {
              if (event.key === 'Enter' && visibleViews[0]) {
                event.preventDefault();
                onOpen(visibleViews[0]);
              } else if (event.key === 'ArrowDown') {
                event.preventDefault();
                dialogRef.current?.querySelector<HTMLButtonElement>('[data-quick-row-main]')?.focus();
              }
            }}
            placeholder="搜索工具、插件或视图"
            aria-label="搜索快捷工具"
          />
          {query ? <button type="button" className="quick-access-dialog-search-clear" title="清空搜索" aria-label="清空搜索" onClick={() => { setQuery(''); searchRef.current?.focus(); }}><X size={14} /></button> : null}
        </div>
        <div className="quick-access-dialog-filters" aria-label="快捷工具筛选">
          {([
            ['all', '全部', views.length],
            ['favorites', '收藏', favoriteViews.length],
            ['frequent', '常用', frequentViews.length],
          ] as const).map(([value, label, count]) => (
            <button
              key={value}
              type="button"
              className={filter === value ? 'active' : ''}
              aria-pressed={filter === value}
              onClick={() => setFilter(value)}
            >
              {label}<span>{count}</span>
            </button>
          ))}
        </div>
        <div className="quick-access-dialog-list" role="list" aria-label="插件工具">
          {visibleViews.map(view => {
            const key = quickAccessKey(view);
            const Icon = quickViewIcon(view.icon);
            const favorite = favorites.has(key);
            const viewUsage = usage[key];
            return (
              <div className="quick-access-dialog-row" role="listitem" key={key} data-quick-access-key={key}>
                <button type="button" className="quick-access-dialog-row-main" data-quick-row-main onClick={() => onOpen(view)}>
                  <span className="quick-access-dialog-row-icon"><Icon size={18} strokeWidth={1.8} aria-hidden="true" /></span>
                  <span className="quick-access-dialog-row-copy"><strong>{quickAccessLabel(view)}</strong><small>{view.plugin_name} · {view.title}</small></span>
                </button>
                <span className="quick-access-dialog-row-meta">
                  {favorite ? <span className="quick-access-dialog-tag favorite"><Star size={11} fill="currentColor" aria-hidden="true" />收藏</span> : null}
                  {viewUsage?.count ? <span className="quick-access-dialog-tag">使用 {viewUsage.count} 次</span> : null}
                </span>
                <button
                  type="button"
                  className={`quick-access-favorite-button${favorite ? ' active' : ''}`}
                  aria-label={favorite ? `取消收藏 ${quickAccessLabel(view)}` : `收藏 ${quickAccessLabel(view)}`}
                  aria-pressed={favorite}
                  title={favorite ? '取消收藏' : '收藏'}
                  onClick={() => onToggleFavorite(view)}
                >
                  <Star size={16} fill={favorite ? 'currentColor' : 'none'} aria-hidden="true" />
                </button>
              </div>
            );
          })}
          {!visibleViews.length ? (
            <div className="quick-access-dialog-empty">
              {filter === 'favorites' ? <Star size={24} aria-hidden="true" /> : filter === 'frequent' ? <Clock3 size={24} aria-hidden="true" /> : <Search size={24} aria-hidden="true" />}
              <strong>{query ? '没有匹配的工具' : filter === 'favorites' ? '还没有收藏' : filter === 'frequent' ? '还没有常用记录' : '没有可用工具'}</strong>
              <span>{query ? '换个关键词，或清空搜索查看全部入口。' : filter === 'favorites' ? '点击工具右侧的星标即可固定到顶部。' : '打开工具后会自动记录使用频次。'}</span>
            </div>
          ) : null}
        </div>
        <footer className="quick-access-dialog-foot">
          <span>{visibleViews.length} / {views.length} 个入口</span>
          <span>Enter 打开首个结果 · Esc 关闭</span>
        </footer>
      </section>
    </div>
  );
}
type MenuKey = 'agent' | 'view' | 'help';

function AppMenuBar({ currentPage, inboxCount, agentVersion, updateBusy, dashboardEnabled, currentTask, activeRunCount, sidebarCollapsed, onToggleSidebar, onNavigate, onOpenDashboard, onOpenBuiltinAi, onCheckUpdate, onOpenAgentDirectory, onQuit }: Pick<ShellProps, 'currentPage' | 'agentVersion' | 'updateBusy' | 'dashboardEnabled' | 'currentTask' | 'activeRunCount' | 'onNavigate' | 'onOpenDashboard' | 'onOpenBuiltinAi' | 'onCheckUpdate' | 'onOpenAgentDirectory' | 'onQuit'> & { inboxCount: number; sidebarCollapsed: boolean; onToggleSidebar: () => void }) {
  const [openMenu, setOpenMenu] = useState<MenuKey | null>(null);
  const [aboutOpen, setAboutOpen] = useState(false);
  const menuBarRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const closeOnPointerDown = (event: PointerEvent) => {
      if (!menuBarRef.current?.contains(event.target as Node)) setOpenMenu(null);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        setOpenMenu(null);
        setAboutOpen(false);
      }
    };
    document.addEventListener('pointerdown', closeOnPointerDown);
    document.addEventListener('keydown', closeOnEscape);
    return () => {
      document.removeEventListener('pointerdown', closeOnPointerDown);
      document.removeEventListener('keydown', closeOnEscape);
    };
  }, []);

  const toggleMenu = (menu: MenuKey) => setOpenMenu(current => current === menu ? null : menu);
  const runAction = (action: () => void) => {
    setOpenMenu(null);
    action();
  };
  useEffect(() => {
    const menuBar = menuBarRef.current;
    if (!menuBar) return;
    const handleTitleBarMouseDown = (event: MouseEvent) => {
      if (event.button !== 0) return;
      const target = event.target;
      if (target instanceof Element && target.closest('button, a, input, select, [role="menu"]')) return;
      // Capture before Tauri's drag-region handler and WebView2's focus handoff.
      // Use Tauri's official Window API so dragging follows the same native path
      // as the framework drag-region implementation.
      event.preventDefault();
      void getCurrentWindow().startDragging().catch(error => console.error('窗口拖拽失败', error));
    };
    menuBar.addEventListener('mousedown', handleTitleBarMouseDown, { capture: true });
    return () => menuBar.removeEventListener('mousedown', handleTitleBarMouseDown, { capture: true });
  }, []);
  const handleTitleBarDoubleClick = (event: ReactMouseEvent<HTMLDivElement>) => {
    const target = event.target;
    if (target instanceof Element && target.closest('button, a, input, select, [role="menu"]')) return;
    void invoke('window_toggle_maximize').catch(error => console.error('窗口最大化失败', error));
  };
  const menuBadge = (item: NavigationItem) => item.badgeKey === 'inbox' && inboxCount > 0 ? <span className="menu-badge">{inboxCount}</span> : null;
  const taskCenterLabel = pageLabel('tasks');
  // 状态入口同时覆盖两条执行线：工作台任务（currentTask）和本机工作流/技能运行。
  // 后者以前只有停在「工作流」页才看得见，切到别的页面就不知道还有东西在跑。
  const busyLabel = currentTask ? '执行中' : activeRunCount > 0 ? `${activeRunCount} 个运行中` : '';
  const statusTitle = currentTask
    ? `${taskCenterLabel} · 正在执行 ${taskTypeLabel(currentTask.task_type)}`
    : activeRunCount > 0
      ? `${taskCenterLabel} · ${activeRunCount} 个任务运行中`
      : `${taskCenterLabel} · 当前没有执行中的任务`;
  const visibleSections = visibleNavigationSections({ dashboardEnabled });
  return (
    <>
      <div className="app-menu-bar" ref={menuBarRef} aria-label="应用菜单" data-tauri-drag-region onDoubleClick={handleTitleBarDoubleClick}>
        <button
          type="button"
          className="app-sidebar-toggle"
          title={sidebarCollapsed ? '展开侧栏' : '收缩侧栏'}
          aria-label={sidebarCollapsed ? '展开侧栏' : '收缩侧栏'}
          aria-pressed={sidebarCollapsed}
          onClick={onToggleSidebar}
        >
          {sidebarCollapsed ? <PanelLeftOpen size={16} aria-hidden="true" /> : <PanelLeftClose size={16} aria-hidden="true" />}
        </button>
        <div className="app-menu-brand" aria-label="HiMind Agent" data-tauri-drag-region>
          <span className="app-menu-brand-mark"><span /></span>
          <strong>HiMind Agent</strong>
        </div>
        <div className="app-menu-group">
          <button type="button" className={openMenu === 'agent' ? 'active' : ''} onClick={() => toggleMenu('agent')} aria-expanded={openMenu === 'agent'}>
            HiMind <ChevronDown size={13} aria-hidden="true" />
          </button>
          {openMenu === 'agent' ? (
            <div className="app-menu-dropdown" role="menu">
              <button type="button" role="menuitem" onClick={() => runAction(onOpenBuiltinAi)}><MessageCircle size={16} /><span>打开 AI 对话</span></button>
              {dashboardEnabled ? <button type="button" role="menuitem" onClick={() => runAction(onOpenDashboard)}><ExternalLink size={16} /><span>打开工作台</span></button> : null}
              <button type="button" role="menuitem" onClick={() => runAction(onOpenAgentDirectory)}><FolderOpen size={16} /><span>打开数据目录</span></button>
              <button type="button" role="menuitem" disabled={updateBusy} onClick={() => runAction(onCheckUpdate)}>{updateBusy ? <BusyIndicator size={16} /> : <RefreshCw size={16} />}<span>{updateBusy ? '正在检查更新' : '检查更新'}</span></button>
              <div className="app-menu-separator" role="separator" />
              <button type="button" role="menuitem" className="danger" onClick={() => runAction(onQuit)}><LogOut size={16} /><span>退出 HiMind Agent</span></button>
            </div>
          ) : null}
        </div>

        <div className="app-menu-group">
          <button type="button" className={openMenu === 'view' ? 'active' : ''} onClick={() => toggleMenu('view')} aria-expanded={openMenu === 'view'}>
            查看 <ChevronDown size={13} aria-hidden="true" />
          </button>
          {openMenu === 'view' ? (
            <div className="app-menu-dropdown" role="menu">
              {visibleSections.map((section, sectionIndex) => <div key={section.id}>
                  {sectionIndex > 0 ? <div className="app-menu-separator" role="separator" /> : null}
                  {section.label ? <div className="app-menu-section-label">{section.label}</div> : null}
                  {section.items.map(item => {
                    const Icon = item.icon;
                    return <button type="button" role="menuitem" key={item.key} onClick={() => runAction(() => onNavigate(item.key))}><Icon size={16} /><span>{item.label}</span>{menuBadge(item)}</button>;
                  })}
                </div>)}
            </div>
          ) : null}
        </div>

        <div className="app-menu-group">
          <button type="button" className={openMenu === 'help' ? 'active' : ''} onClick={() => toggleMenu('help')} aria-expanded={openMenu === 'help'}>
            帮助 <ChevronDown size={13} aria-hidden="true" />
          </button>
          {openMenu === 'help' ? (
            <div className="app-menu-dropdown" role="menu">
              <button type="button" role="menuitem" onClick={() => { setOpenMenu(null); setAboutOpen(true); }}><Info size={16} /><span>关于 HiMind</span></button>
            </div>
          ) : null}
        </div>
        <div className="app-menu-drag-space" aria-hidden="true" data-tauri-drag-region />
        <button
          type="button"
          className={`app-status-chip${busyLabel ? ' running' : ''}${currentPage === 'tasks' ? ' active' : ''}`}
          onClick={() => runAction(() => onNavigate('tasks'))}
          title={statusTitle}
          aria-label={busyLabel ? `${taskCenterLabel}，${busyLabel}` : taskCenterLabel}
        >
          <span className={`app-status-chip-icon${busyLabel ? ' is-live' : ''}`} aria-hidden="true">
            <MorphIcon icon={busyLabel ? loaderCircleIconData : activityIconData} size={15} strokeWidth={1.9} />
          </span>
          {busyLabel ? <span>{busyLabel}</span> : null}
        </button>
        <div className="app-window-controls" aria-label="窗口控制">
          <button type="button" className="app-window-control" title="最小化" aria-label="最小化" onClick={() => { void invoke('window_minimize').catch(error => console.error('窗口最小化失败', error)); }}><Minus size={14} /></button>
          <button type="button" className="app-window-control" title="最大化或还原" aria-label="最大化或还原" onClick={() => { void invoke('window_toggle_maximize').catch(error => console.error('窗口最大化失败', error)); }}><Square size={11} /></button>
          <button type="button" className="app-window-control close" title="关闭窗口" aria-label="关闭窗口" onClick={() => { void invoke('window_close').catch(error => console.error('窗口关闭失败', error)); }}><X size={14} /></button>
        </div>
      </div>

      {aboutOpen ? (
        <div className="modal-backdrop app-about-backdrop" role="presentation" onClick={event => { if (event.currentTarget === event.target) setAboutOpen(false); }}>
          <section className="modal app-about-modal" role="dialog" aria-modal="true" aria-labelledby="app-about-title">
            <button type="button" className="app-about-close" title="关闭" aria-label="关闭" onClick={() => setAboutOpen(false)}><X size={16} /></button>
            <img src="/brand/himind-app.png" alt="" aria-hidden="true" />
            <h2 id="app-about-title">HiMind Agent</h2>
            <span className="app-about-version">版本 {agentVersion}</span>
            <p>为 AI 工作台提供本机执行能力。</p>
            <button type="button" className="btn btn-primary" onClick={() => setAboutOpen(false)}>确定</button>
          </section>
        </div>
      ) : null}
    </>
  );
}

export function Shell({ currentPage, approvalCount, workflowApprovalCount, identity, dashboardEnabled, agentVersion, updateBusy, currentTask, activeRunCount, activeRun, quickPluginViews, onNavigate, onOpenPluginView, onOpenSettings, onOpenDashboard, onOpenBuiltinAi, onCheckUpdate, onOpenAgentDirectory, onQuit, children }: ShellProps) {
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => {
    try {
      return window.localStorage.getItem('himind.sidebar.collapsed') === '1';
    } catch {
      return false;
    }
  });
  const [accountMenuOpen, setAccountMenuOpen] = useState(false);
  const [quickAccessOpen, setQuickAccessOpen] = useState(false);
  const [favoriteQuickAccessKeys, setFavoriteQuickAccessKeys] = useState<string[]>(readQuickAccessFavorites);
  const [quickAccessUsage, setQuickAccessUsage] = useState<QuickAccessUsage>(readQuickAccessUsage);
  const sidebarNavRef = useRef<HTMLElement>(null);
  const accountMenuRef = useRef<HTMLDivElement>(null);
  const quickAccessTriggerRef = useRef<HTMLButtonElement>(null);
  const favoriteQuickAccessSet = useMemo(() => new Set(favoriteQuickAccessKeys), [favoriteQuickAccessKeys]);
  const normalizedQuickPluginViews = useMemo(() => dedupeQuickAccessViews(quickPluginViews), [quickPluginViews]);
  const visibleQuickPluginViews = useMemo(() => {
    const ordered = [
      ...normalizedQuickPluginViews.filter(view => favoriteQuickAccessSet.has(quickAccessKey(view))),
      ...normalizedQuickPluginViews
        .filter(view => (quickAccessUsage[quickAccessKey(view)]?.count || 0) > 0)
        .sort((left, right) => {
          const leftUsage = quickAccessUsage[quickAccessKey(left)];
          const rightUsage = quickAccessUsage[quickAccessKey(right)];
          return rightUsage.count - leftUsage.count
            || rightUsage.lastUsed - leftUsage.lastUsed
            || compareQuickViews(left, right);
        }),
      ...[...normalizedQuickPluginViews].sort(compareQuickViews),
    ];
    return pickUniqueQuickAccessLabels(ordered, QUICK_ACCESS_VISIBLE_LIMIT);
  }, [favoriteQuickAccessSet, normalizedQuickPluginViews, quickAccessUsage]);
  const quickAccessCountLabel = normalizedQuickPluginViews.length > 99 ? '99+' : String(normalizedQuickPluginViews.length);
  // 常驻运行条优先跟随工作台任务；没有工作台任务时跟随本机在跑的第一个运行，
  // 这样未连接工作台（独立模式）时也能看到「有东西正在执行」和已运行时长。
  const localRun = dashboardEnabled && currentTask ? null : activeRun;
  const localRunElapsed = useRunElapsed(localRun?.startedAt);
  useEffect(() => {
    const nav = sidebarNavRef.current;
    if (!nav) return;
    nav.querySelector<HTMLButtonElement>('button.active')?.scrollIntoView({ block: 'nearest' });
  }, [currentPage, dashboardEnabled]);
  useEffect(() => {
    try {
      window.localStorage.setItem('himind.sidebar.collapsed', sidebarCollapsed ? '1' : '0');
    } catch {
      // Local storage may be unavailable in restricted webview profiles.
    }
  }, [sidebarCollapsed]);
  useEffect(() => {
    try {
      window.localStorage.setItem(QUICK_ACCESS_FAVORITES_KEY, JSON.stringify(favoriteQuickAccessKeys));
    } catch {
      // Favorites remain usable in memory if the webview blocks storage.
    }
  }, [favoriteQuickAccessKeys]);
  useEffect(() => {
    try {
      window.localStorage.setItem(QUICK_ACCESS_USAGE_KEY, JSON.stringify(quickAccessUsage));
    } catch {
      // Usage ordering remains usable in memory if the webview blocks storage.
    }
  }, [quickAccessUsage]);
  useEffect(() => {
    const closeOnPointerDown = (event: PointerEvent) => {
      if (!accountMenuRef.current?.contains(event.target as Node)) setAccountMenuOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setAccountMenuOpen(false);
    };
    document.addEventListener('pointerdown', closeOnPointerDown);
    document.addEventListener('keydown', closeOnEscape);
    return () => {
      document.removeEventListener('pointerdown', closeOnPointerDown);
      document.removeEventListener('keydown', closeOnEscape);
    };
  }, []);
  useEffect(() => {
    // 桌面 WebView 不应把浏览器的默认菜单带进应用；编辑控件仍保留
    // 系统菜单，避免复制、粘贴和输入法操作退化。
    const suppressBrowserContextMenu = (event: MouseEvent) => {
      const target = event.target;
      if (!(target instanceof Element)) return;
      if (target.closest('input, textarea, select, [contenteditable="true"]')) return;
      event.preventDefault();
    };
    document.addEventListener('contextmenu', suppressBrowserContextMenu, true);
    return () => document.removeEventListener('contextmenu', suppressBrowserContextMenu, true);
  }, []);

  const runAccountAction = (action: () => void) => {
    setAccountMenuOpen(false);
    action();
  };
  const recordQuickAccessUse = (view: PluginQuickAccessView) => {
    const key = quickAccessKey(view);
    setQuickAccessUsage(current => {
      const previous = current[key];
      return {
        ...current,
        [key]: {
          count: Math.min((previous?.count || 0) + 1, 9999),
          lastUsed: Date.now(),
        },
      };
    });
  };
  const openQuickAccessView = (view: PluginQuickAccessView) => {
    recordQuickAccessUse(view);
    setQuickAccessOpen(false);
    onOpenPluginView(view.plugin_id, view.view_id);
  };
  const toggleQuickAccessFavorite = (view: PluginQuickAccessView) => {
    const key = quickAccessKey(view);
    setFavoriteQuickAccessKeys(current => current.includes(key) ? current.filter(item => item !== key) : [...current, key]);
  };
  const openDashboardAccount = () => {
    onNavigate('dashboard');
    window.setTimeout(() => document.getElementById('account-authorization')?.scrollIntoView({ behavior: 'smooth', block: 'start' }), 0);
  };
  // The rail is narrow, so the account row keeps a short name and a one-word
  // state while the wider account popover carries the fuller wording.
  const accountName = dashboardEnabled ? (identity?.authorized ? identity.user_name || 'HiMind 账号' : 'HiMind 账号') : '本机运行';
  const accountStatus = dashboardEnabled ? (identity?.authorized ? '已连接' : '未连接') : '服务就绪';
  const accountDetail = dashboardEnabled ? (identity?.authorized ? '当前工作台账号' : '尚未连接工作台账号') : '本机服务已就绪';
  const accountHealthy = !dashboardEnabled || Boolean(identity?.authorized);
  const accountInitial = dashboardEnabled && identity?.authorized ? (identity.user_name || '').trim().slice(0, 1) : '';

  return (
    <div className="shell">
      <AppMenuBar currentPage={currentPage} inboxCount={approvalCount + workflowApprovalCount} agentVersion={agentVersion} updateBusy={updateBusy} dashboardEnabled={dashboardEnabled} currentTask={currentTask} activeRunCount={activeRunCount} sidebarCollapsed={sidebarCollapsed} onToggleSidebar={() => setSidebarCollapsed(current => !current)} onNavigate={onNavigate} onOpenDashboard={onOpenDashboard} onOpenBuiltinAi={onOpenBuiltinAi} onCheckUpdate={onCheckUpdate} onOpenAgentDirectory={onOpenAgentDirectory} onQuit={onQuit} />
      <div className="shell-body">
        <aside className={`sidebar${sidebarCollapsed ? ' collapsed' : ''}`}>
        <nav ref={sidebarNavRef} aria-label="主导航">
          {visibleNavigationSections({ dashboardEnabled }).map(section => <div className="sidebar-nav-group" key={section.id}>
            {section.label ? <span className="sidebar-section-label">{section.label}</span> : null}
            {section.items.map(item => <button
              type="button"
              key={item.key}
              className={currentPage === item.key ? 'active' : ''}
              onClick={() => onNavigate(item.key)}
              aria-current={currentPage === item.key ? 'page' : undefined}
              aria-label={item.label}
              title={item.label}
            >
              <item.icon size={17} strokeWidth={1.8} aria-hidden="true" />
              <span>{item.label}</span>
              {item.badgeKey === 'inbox' && approvalCount + workflowApprovalCount > 0 ? <span className="badge">{approvalCount + workflowApprovalCount}</span> : null}
              {item.key === 'workflows' && activeRunCount > 0 ? <span className="nav-live-badge" title={`${activeRunCount} 个任务运行中`}><span className="nav-live-dot" aria-hidden="true" />{activeRunCount}</span> : null}
            </button>)}
          </div>)}
        </nav>
        <div className="sidebar-footer" ref={accountMenuRef}>
          <button
            className={`sidebar-account ${dashboardEnabled && identity?.authorized ? 'authorized' : ''}${!dashboardEnabled ? ' independent-account' : ''}${accountMenuOpen ? ' open' : ''}`}
            type="button"
            onClick={() => setAccountMenuOpen(current => !current)}
            title="打开账号菜单"
            aria-label="打开账号菜单"
            aria-haspopup="menu"
            aria-expanded={accountMenuOpen}
            aria-controls="sidebar-account-menu"
          >
            <span className="sidebar-account-avatar" aria-hidden="true">{accountInitial || (dashboardEnabled ? <CircleUserRound size={16} /> : <CheckCircle2 size={16} />)}</span>
            <span className="sidebar-account-copy">
              <strong>{accountName}</strong>
              <small><span className={`status-dot sidebar-account-status ${accountHealthy ? 'success' : ''}`} />{accountStatus}</small>
            </span>
            <ChevronDown className="sidebar-account-chevron" size={14} aria-hidden="true" />
          </button>
          {accountMenuOpen ? (
            <div id="sidebar-account-menu" className="sidebar-account-menu" role="menu" aria-label="账号菜单">
              <div className="sidebar-account-menu-summary">
                <span className={`status-dot ${accountHealthy ? 'success' : ''}`} />
                <span><strong>{accountName}</strong><small>{accountDetail}</small></span>
              </div>
              <div className="sidebar-account-menu-separator" role="separator" />
              {dashboardEnabled ? <button type="button" role="menuitem" onClick={() => runAccountAction(openDashboardAccount)}>
                <CircleUserRound size={17} aria-hidden="true" />
                <span><strong>账号与凭据</strong><small>管理工作台、插件和服务凭据</small></span>
              </button> : null}
              <button type="button" role="menuitem" className={currentPage === 'settings' || currentPage === 'ai' || currentPage === 'logs' ? 'active' : ''} onClick={() => runAccountAction(onOpenSettings)}>
                <Settings size={17} aria-hidden="true" />
                <span><strong>设置</strong><small>连接、权限与运行状态</small></span>
              </button>
            </div>
          ) : null}
        </div>
        </aside>
        <main className={`main${currentPage === 'builtin-ai' ? ' builtin-ai-main' : ''}`}>
          {normalizedQuickPluginViews.length ? (
            <div className="quick-access-bar" aria-label="快捷入口">
              <span className="quick-access-caption">快捷工具</span>
              <div className="quick-access-items" role="toolbar" aria-label="插件快捷入口">
                {visibleQuickPluginViews.map(view => {
                  const Icon = quickViewIcon(view.icon);
                  const label = quickAccessLabel(view);
                  const key = quickAccessKey(view);
                  const favorite = favoriteQuickAccessSet.has(key);
                  const accessibleLabel = `${view.plugin_name} · ${view.title}`;
                  return (
                    <button
                      type="button"
                      key={key}
                      className={`quick-access-button${favorite ? ' is-favorite' : ''}`}
                      data-quick-access-key={key}
                      onClick={() => openQuickAccessView(view)}
                      aria-label={accessibleLabel}
                      title={favorite ? `已收藏 · ${accessibleLabel}` : accessibleLabel}
                    >
                      <Icon size={17} strokeWidth={1.8} aria-hidden="true" />
                      <span className="quick-access-button-label">{label}</span>
                      {favorite ? <Star className="quick-access-favorite-mark" size={10} fill="currentColor" aria-hidden="true" /> : null}
                    </button>
                  );
                })}
              </div>
              <button
                ref={quickAccessTriggerRef}
                type="button"
                className="quick-access-button quick-access-all-button"
                onClick={() => setQuickAccessOpen(true)}
                aria-label="打开全部工具并搜索"
                aria-haspopup="dialog"
                aria-expanded={quickAccessOpen}
                title="搜索、收藏和管理全部插件工具"
              >
                <Search size={17} strokeWidth={1.8} aria-hidden="true" />
                <span className="quick-access-button-label">全部工具</span>
                <span className="quick-access-count" aria-hidden="true">{quickAccessCountLabel}</span>
              </button>
            </div>
          ) : null}
          {quickAccessOpen ? (
            <QuickAccessDialog
              views={normalizedQuickPluginViews}
              favorites={favoriteQuickAccessSet}
              usage={quickAccessUsage}
              onToggleFavorite={toggleQuickAccessFavorite}
              onOpen={openQuickAccessView}
              onClose={() => setQuickAccessOpen(false)}
              returnFocusRef={quickAccessTriggerRef}
            />
          ) : null}
          <div className="main-content">
            {currentPage === 'builtin-ai' ? null : dashboardEnabled && currentTask ? (
              <button type="button" className="current-task-strip" onClick={() => onNavigate('tasks')} title="查看我的任务">
                <BusyIndicator size={15} />
                <span><strong>正在执行 {taskTypeLabel(currentTask.task_type)}</strong><small>{currentTask.task_id}</small></span>
                <code>{currentTask.execution_id || '本机执行'}</code>
                <span className="current-task-open-label">查看我的任务</span>
              </button>
            ) : localRun ? (
              // 未连接工作台时同样要有运行态常驻反馈：本机工作流/技能运行也是一种「正在执行」。
              <button type="button" className="current-task-strip" onClick={() => onNavigate('tasks')} title="查看我的任务">
                <BusyIndicator size={15} />
                <span>
                  <strong>正在执行 {localRun.title}</strong>
                  <small>{localRun.stage}{localRun.count > 1 ? ` · 还有 ${localRun.count - 1} 个在跑` : ''}{localRunElapsed !== null ? ` · 已运行 ${formatElapsedCn(localRunElapsed)}` : ''}</small>
                </span>
                <code title={localRun.runId}>{shortRunId(localRun.runId)}</code>
                <span className="current-task-open-label">查看我的任务</span>
              </button>
            ) : null}
            {children}
          </div>
        </main>
      </div>
    </div>
  );
}
