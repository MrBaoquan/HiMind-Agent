import { useEffect, useRef, useState, type ReactNode } from 'react';
import { AppWindow, Blocks, BookOpen, Cable, CheckCircle2, ChevronDown, CircleAlert, CircleUserRound, Clapperboard, Clock3, Database, ExternalLink, FileCode2, FileText, FolderOpen, Info, LayoutDashboard, LayoutGrid, ListChecks, LoaderCircle, LogOut, MessageCircle, MonitorPlay, Music, Package, PanelLeftClose, PanelLeftOpen, Puzzle, RefreshCw, Settings, Sparkles, Terminal, Video, Workflow, Wrench, X, type LucideIcon } from 'lucide-react';
import type { NavigationTarget, PageKey } from '../types';
import { pageLabel, visibleNavigationSections, type NavigationItem } from '../navigation';
import type { CurrentTaskStatus, DashboardIdentityStatus, PluginQuickAccessView } from '../services/agentApi';
import { taskTypeLabel } from '../pages/taskView';

type ShellProps = {
  currentPage: PageKey;
  approvalCount: number;
  workflowApprovalCount: number;
  identity: DashboardIdentityStatus | null;
  dashboardEnabled: boolean;
  agentVersion: string;
  updateBusy: boolean;
  currentTask: CurrentTaskStatus | null;
  quickPluginViews: PluginQuickAccessView[];
  onNavigate: (target: NavigationTarget) => void;
  onOpenPluginView: (pluginId: string, viewId: string) => void;
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

type MenuKey = 'agent' | 'view' | 'tools' | 'help';

function AppMenuBar({ currentPage, inboxCount, agentVersion, updateBusy, dashboardEnabled, onNavigate, onOpenDashboard, onOpenBuiltinAi, onCheckUpdate, onOpenAgentDirectory, onOpenTasks, onQuit }: Pick<ShellProps, 'currentPage' | 'agentVersion' | 'updateBusy' | 'dashboardEnabled' | 'onNavigate' | 'onOpenDashboard' | 'onOpenBuiltinAi' | 'onCheckUpdate' | 'onOpenAgentDirectory' | 'onQuit'> & { inboxCount: number; onOpenTasks: () => void }) {
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
  const menuBadge = (item: NavigationItem) => item.badgeKey === 'inbox' && inboxCount > 0 ? <span className="menu-badge">{inboxCount}</span> : null;
  const visibleSections = visibleNavigationSections({ dashboardEnabled });
  return (
    <>
      <div className="app-menu-bar" ref={menuBarRef} aria-label="应用菜单">
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
              {visibleSections.map((section, sectionIndex) => (
                <div key={section.id}>
                  {sectionIndex > 0 ? <div className="app-menu-separator" role="separator" /> : null}
                  {section.items.map(item => {
                    const Icon = item.icon;
                    return <button type="button" role="menuitem" key={item.key} onClick={() => runAction(() => onNavigate(item.key))}><Icon size={16} /><span>{item.label}</span>{menuBadge(item)}</button>;
                  })}
                </div>
              ))}
            </div>
          ) : null}
        </div>

        <div className="app-menu-group">
          <button type="button" className={openMenu === 'tools' ? 'active' : ''} onClick={() => toggleMenu('tools')} aria-expanded={openMenu === 'tools'}>
            工具 <ChevronDown size={13} aria-hidden="true" />
          </button>
          {openMenu === 'tools' ? (
            <div className="app-menu-dropdown" role="menu">
              {dashboardEnabled ? <button type="button" role="menuitem" onClick={() => runAction(onOpenTasks)}><ListChecks size={16} /><span>任务中心</span></button> : null}
              <button type="button" role="menuitem" disabled={updateBusy} onClick={() => runAction(onCheckUpdate)}><RefreshCw className={updateBusy ? 'spin' : ''} size={16} /><span>{updateBusy ? '正在检查更新' : '检查更新'}</span></button>
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
        <span className="app-menu-context" title={`当前位置：${pageLabel(currentPage)}`}>{pageLabel(currentPage)}</span>
      </div>

      {aboutOpen ? (
        <div className="modal-backdrop app-about-backdrop" role="presentation" onClick={event => { if (event.currentTarget === event.target) setAboutOpen(false); }}>
          <section className="modal app-about-modal" role="dialog" aria-modal="true" aria-labelledby="app-about-title">
            <button type="button" className="app-about-close" title="关闭" aria-label="关闭" onClick={() => setAboutOpen(false)}><X size={16} /></button>
            <img src="/brand/himind-app.png" alt="" aria-hidden="true" />
            <h2 id="app-about-title">HiMind Agent</h2>
            <span className="app-about-version">版本 {agentVersion}</span>
            <p>为 HiMind 工作台提供本机执行能力。</p>
            <button type="button" className="btn btn-primary" onClick={() => setAboutOpen(false)}>确定</button>
          </section>
        </div>
      ) : null}
    </>
  );
}

export function Shell({ currentPage, approvalCount, workflowApprovalCount, identity, dashboardEnabled, agentVersion, updateBusy, currentTask, quickPluginViews, onNavigate, onOpenPluginView, onOpenDashboard, onOpenBuiltinAi, onCheckUpdate, onOpenAgentDirectory, onQuit, children }: ShellProps) {
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => {
    try {
      return window.localStorage.getItem('himind.sidebar.collapsed') === '1';
    } catch {
      return false;
    }
  });
  useEffect(() => {
    try {
      window.localStorage.setItem('himind.sidebar.collapsed', sidebarCollapsed ? '1' : '0');
    } catch {
      // Local storage may be unavailable in restricted webview profiles.
    }
  }, [sidebarCollapsed]);

  return (
    <div className="shell">
      <AppMenuBar currentPage={currentPage} inboxCount={approvalCount + workflowApprovalCount} agentVersion={agentVersion} updateBusy={updateBusy} dashboardEnabled={dashboardEnabled} onNavigate={onNavigate} onOpenDashboard={onOpenDashboard} onOpenBuiltinAi={onOpenBuiltinAi} onCheckUpdate={onCheckUpdate} onOpenAgentDirectory={onOpenAgentDirectory} onOpenTasks={() => onNavigate('tasks')} onQuit={onQuit} />
      <div className="shell-body">
        <aside className={`sidebar${sidebarCollapsed ? ' collapsed' : ''}`}>
        <div className="sidebar-header">
          <img className="product-mark" src="/brand/himind-app.png" alt="" aria-hidden="true" />
          <div>
            <h1>HiMind</h1>
            <div className="product-type">桌面端</div>
          </div>
          <button
            type="button"
            className="sidebar-collapse-toggle"
            title={sidebarCollapsed ? '展开侧栏' : '收缩侧栏'}
            aria-label={sidebarCollapsed ? '展开侧栏' : '收缩侧栏'}
            aria-pressed={sidebarCollapsed}
            onClick={() => setSidebarCollapsed(current => !current)}
          >
            {sidebarCollapsed ? <PanelLeftOpen size={16} aria-hidden="true" /> : <PanelLeftClose size={16} aria-hidden="true" />}
          </button>
        </div>
        <button
          type="button"
          className={`sidebar-ai-entry ${currentPage === 'builtin-ai' ? 'active' : ''}`}
          onClick={onOpenBuiltinAi}
          aria-current={currentPage === 'builtin-ai' ? 'page' : undefined}
          aria-label="打开 HiMind AI"
          title="打开 HiMind AI"
        >
          <MessageCircle size={17} strokeWidth={1.8} aria-hidden="true" />
          <span>HiMind AI</span>
        </button>
        <nav aria-label="主导航">
          {visibleNavigationSections({ dashboardEnabled }).map(section => (
            <div className="sidebar-nav-group" key={section.id}>
              <span className="sidebar-section-label">{section.label}</span>
              {section.items.map(item => (
                <button
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
                </button>
              ))}
            </div>
          ))}
        </nav>
        <div className="sidebar-footer">
          {dashboardEnabled ? <button className={`sidebar-account ${identity?.authorized ? 'authorized' : ''}`} type="button" onClick={() => { onNavigate('dashboard'); window.setTimeout(() => document.getElementById('account-authorization')?.scrollIntoView({ behavior: 'smooth', block: 'start' }), 0); }} title="HiMind 账号">
            <CircleUserRound size={18} />
            <span><strong>{identity?.authorized ? identity.user_name || 'HiMind 账号' : 'HiMind 账号未连接'}</strong><small>{identity?.authorized ? '账号已连接' : '连接 HiMind 账号'}</small></span>
            <span className={`status-dot sidebar-account-status ${identity?.authorized ? 'success' : ''}`} />
          </button> : <div className="sidebar-account independent-account" title="本机服务状态">
            <CheckCircle2 size={18} />
            <span><strong>独立运行</strong><small>本机服务已就绪</small></span>
            <span className="status-dot sidebar-account-status success" />
          </div>}
        </div>
        </aside>
        <main className={`main${currentPage === 'builtin-ai' ? ' builtin-ai-main' : ''}`}>
          {quickPluginViews.length ? (
            <div className="quick-access-bar" aria-label="快捷入口">
              <span className="quick-access-caption">快捷工具</span>
              <div className="quick-access-items" role="toolbar" aria-label="插件快捷入口">
                {quickPluginViews.map(view => {
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
              </div>
            </div>
          ) : null}
          <div className="main-content">
            {dashboardEnabled && currentTask && currentPage !== 'builtin-ai' ? <button type="button" className="current-task-strip" onClick={() => onNavigate('tasks')} title="在任务中心查看"><LoaderCircle size={15} className="spin" /><span><strong>正在执行 {taskTypeLabel(currentTask.task_type)}</strong><small>{currentTask.task_id}</small></span><code>{currentTask.execution_id || '本机执行'}</code><span className="current-task-open-label">任务中心</span></button> : null}
            {children}
          </div>
        </main>
      </div>
    </div>
  );
}
