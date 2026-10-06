import { useEffect, useRef, useState, type MouseEvent as ReactMouseEvent, type ReactNode } from 'react';
import { Activity, AppWindow, Blocks, BookOpen, Cable, CheckCircle2, ChevronDown, CircleAlert, CircleUserRound, Clapperboard, Clock3, Database, ExternalLink, FileCode2, FileText, FolderOpen, Info, LayoutDashboard, LayoutGrid, LogOut, MessageCircle, Minus, MonitorPlay, Music, Package, PanelLeftClose, PanelLeftOpen, Puzzle, RefreshCw, Settings, Sparkles, Square, Terminal, Video, Workflow, Wrench, X, type LucideIcon } from 'lucide-react';
// 形变吃的是图标数据，静态图标仍走 lucide-react 组件；两边的图标集版本必须一致，
// 否则同一个图标在「形变端点」和「静态渲染」下长得不一样（由 check:icon-version 守住）。
import { Activity as activityIconData, LoaderCircle as loaderCircleIconData } from 'lucide';
import { BusyIndicator } from './BusyIndicator';
import { MorphIcon } from './MorphIcon';
import type { NavigationTarget, PageKey } from '../types';
import { pageLabel, visibleNavigationSections, type NavigationItem } from '../navigation';
import type { CurrentTaskStatus, DashboardIdentityStatus, PluginQuickAccessView } from '../services/agentApi';
import { shortRunId, taskElapsedSeconds, taskTypeLabel } from '../pages/taskView';
import { formatElapsedCn } from '../pages/workflowRunView';
import { invoke } from '@tauri-apps/api/core';

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
  clapperboard: Clapperboard,
  code: FileCode2,
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

function quickViewLabel(view: PluginQuickAccessView) {
  const label = view.short_title?.trim() || view.plugin_name?.trim() || view.title.trim();
  return label || '工具';
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
  const handleTitleBarMouseDown = (event: ReactMouseEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    const target = event.target;
    if (target instanceof Element && target.closest('button, a, input, select, [role="menu"]')) return;
    void invoke('window_start_dragging').catch(error => console.error('窗口拖拽失败', error));
  };
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
      <div className="app-menu-bar" ref={menuBarRef} aria-label="应用菜单" data-tauri-drag-region onMouseDown={handleTitleBarMouseDown} onDoubleClick={handleTitleBarDoubleClick}>
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
        <div className="app-menu-brand" aria-label="HiMind Agent">
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
        <div className="app-menu-drag-space" aria-hidden="true" />
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
  const sidebarNavRef = useRef<HTMLElement>(null);
  const accountMenuRef = useRef<HTMLDivElement>(null);
  const visibleQuickPluginViews = quickPluginViews.slice(0, 4);
  const hasMoreQuickPluginViews = quickPluginViews.length > visibleQuickPluginViews.length;
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

  const runAccountAction = (action: () => void) => {
    setAccountMenuOpen(false);
    action();
  };
  const openDashboardAccount = () => {
    onNavigate('dashboard');
    window.setTimeout(() => document.getElementById('account-authorization')?.scrollIntoView({ behavior: 'smooth', block: 'start' }), 0);
  };
  // The rail is narrow, so the account row keeps a short name and a one-word
  // state while the wider account popover carries the fuller wording.
  const accountName = dashboardEnabled ? (identity?.authorized ? identity.user_name || 'HiMind 账号' : 'HiMind 账号') : '独立运行';
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
                <span><strong>账号与工作台</strong><small>管理账号连接和工作区</small></span>
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
          {quickPluginViews.length ? (
            <div className="quick-access-bar" aria-label="快捷入口">
              <span className="quick-access-caption">快捷工具</span>
              <div className="quick-access-items" role="toolbar" aria-label="插件快捷入口">
                {visibleQuickPluginViews.map(view => {
                  const Icon = quickViewIcon(view.icon);
                  const label = quickViewLabel(view);
                  const accessibleLabel = `${view.plugin_name} · ${view.title}`;
                  return (
                    <button
                      type="button"
                      key={`${view.plugin_id}:${view.view_id}`}
                      className="quick-access-button"
                      onClick={() => onOpenPluginView(view.plugin_id, view.view_id)}
                      aria-label={accessibleLabel}
                      title={accessibleLabel}
                    >
                      <Icon size={17} strokeWidth={1.8} aria-hidden="true" />
                      <span className="quick-access-button-label">{label}</span>
                    </button>
                  );
                })}
                {hasMoreQuickPluginViews ? <button type="button" className="quick-access-button" onClick={() => onNavigate({ page: 'installed', kind: 'plugin' })} aria-label="打开更多插件工具" title="打开更多插件工具"><LayoutGrid size={17} strokeWidth={1.8} aria-hidden="true" /><span className="quick-access-button-label">更多工具</span></button> : null}
              </div>
            </div>
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
