import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { AppWindow, Blocks, BookOpen, Cable, CheckCircle2, ChevronDown, CircleAlert, CircleUserRound, Clapperboard, Clock3, Database, ExternalLink, FileCode2, FileText, FolderOpen, Info, LayoutDashboard, LayoutGrid, ListChecks, LoaderCircle, LogOut, MessageCircle, MonitorPlay, Music, Package, PanelLeftClose, PanelLeftOpen, Puzzle, RefreshCw, Settings, Sparkles, Terminal, Video, Workflow, Wrench, X, type LucideIcon } from 'lucide-react';
import type { NavigationTarget, PageKey } from '../types';
import { pageLabel, visibleNavigationSections, type NavigationItem } from '../navigation';
import type { AgentTaskHistoryItem, CurrentTaskStatus, DashboardIdentityStatus, PluginQuickAccessView } from '../services/agentApi';

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
  onLoadTaskHistory: () => Promise<AgentTaskHistoryItem[]>;
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
              {dashboardEnabled ? <button type="button" role="menuitem" onClick={() => runAction(onOpenTasks)}><ListChecks size={16} /><span>任务记录</span></button> : null}
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

export function Shell({ currentPage, approvalCount, workflowApprovalCount, identity, dashboardEnabled, agentVersion, updateBusy, currentTask, quickPluginViews, onLoadTaskHistory, onNavigate, onOpenPluginView, onOpenDashboard, onOpenBuiltinAi, onCheckUpdate, onOpenAgentDirectory, onQuit, children }: ShellProps) {
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => {
    try {
      return window.localStorage.getItem('himind.sidebar.collapsed') === '1';
    } catch {
      return false;
    }
  });
  const [taskDrawerOpen, setTaskDrawerOpen] = useState(false);
  const [taskHistory, setTaskHistory] = useState<AgentTaskHistoryItem[]>([]);
  const [taskHistoryLoading, setTaskHistoryLoading] = useState(false);
  const [taskHistoryError, setTaskHistoryError] = useState('');
  const loadTaskHistory = useCallback(async (silent = false) => {
    if (!silent) setTaskHistoryLoading(true);
    try {
      setTaskHistory(await onLoadTaskHistory());
      setTaskHistoryError('');
    } catch (error) {
      setTaskHistoryError(typeof error === 'string' ? error : '暂时无法读取任务记录。');
    } finally {
      if (!silent) setTaskHistoryLoading(false);
    }
  }, [onLoadTaskHistory]);

  useEffect(() => {
    if (!taskDrawerOpen) return;
    void loadTaskHistory();
    const timer = window.setInterval(() => void loadTaskHistory(true), 5000);
    return () => window.clearInterval(timer);
  }, [loadTaskHistory, taskDrawerOpen]);

  useEffect(() => {
    try {
      window.localStorage.setItem('himind.sidebar.collapsed', sidebarCollapsed ? '1' : '0');
    } catch {
      // Local storage may be unavailable in restricted webview profiles.
    }
  }, [sidebarCollapsed]);

  return (
    <div className="shell">
      <AppMenuBar currentPage={currentPage} inboxCount={approvalCount + workflowApprovalCount} agentVersion={agentVersion} updateBusy={updateBusy} dashboardEnabled={dashboardEnabled} onNavigate={onNavigate} onOpenDashboard={onOpenDashboard} onOpenBuiltinAi={onOpenBuiltinAi} onCheckUpdate={onCheckUpdate} onOpenAgentDirectory={onOpenAgentDirectory} onOpenTasks={() => setTaskDrawerOpen(true)} onQuit={onQuit} />
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
            {dashboardEnabled && currentTask && currentPage !== 'builtin-ai' ? <button type="button" className="current-task-strip" onClick={() => setTaskDrawerOpen(true)} title="查看当前任务"><LoaderCircle size={15} className="spin" /><span><strong>正在执行 {taskTypeLabel(currentTask.task_type)}</strong><small>{currentTask.task_id}</small></span><code>{currentTask.execution_id || '本机执行'}</code><span className="current-task-open-label">任务记录</span></button> : null}
            {children}
          </div>
        </main>
      </div>
      {taskDrawerOpen ? <TaskHistoryDrawer currentTask={currentTask} items={taskHistory} loading={taskHistoryLoading} error={taskHistoryError} onRefresh={() => void loadTaskHistory()} onClose={() => setTaskDrawerOpen(false)} /> : null}
    </div>
  );
}

function TaskHistoryDrawer({ currentTask, items, loading, error, onRefresh, onClose }: { currentTask: CurrentTaskStatus | null; items: AgentTaskHistoryItem[]; loading: boolean; error: string; onRefresh: () => void; onClose: () => void }) {
  const active = items.filter(item => ['pending', 'running', 'canceling'].includes(item.status));
  const completed = items.filter(item => !['pending', 'running', 'canceling'].includes(item.status));
  return <>
    <button type="button" className="task-drawer-backdrop" aria-label="关闭任务记录" onClick={onClose} />
    <aside className="task-drawer" role="dialog" aria-modal="true" aria-labelledby="task-drawer-title">
      <header className="task-drawer-header"><div><span className="task-drawer-kicker">本机任务</span><h2 id="task-drawer-title">任务记录</h2></div><div className="task-drawer-actions"><button type="button" className="btn btn-icon" title="刷新任务记录" aria-label="刷新任务记录" onClick={onRefresh} disabled={loading}><RefreshCw size={16} className={loading ? 'spin' : ''} /></button><button type="button" className="btn btn-icon" title="关闭任务记录" aria-label="关闭任务记录" onClick={onClose}><X size={17} /></button></div></header>
      {currentTask ? <section className="task-current-summary"><div className="task-summary-icon"><LoaderCircle size={17} className="spin" /></div><div><strong>正在执行 · {taskTypeLabel(currentTask.task_type)}</strong></div><span className="task-status-pill running">运行中</span></section> : null}
      {error ? <div className="task-drawer-notice"><CircleAlert size={16} /><span>{error}</span></div> : null}
      <div className="task-drawer-body">
        <TaskHistorySection title="进行中" icon={<Clock3 size={15} />} items={active} empty="当前没有进行中的任务。" />
        <TaskHistorySection title="已完成" icon={<CheckCircle2 size={15} />} items={completed} empty="还没有已完成的任务。" />
      </div>
    </aside>
  </>;
}

function TaskHistorySection({ title, icon, items, empty }: { title: string; icon: ReactNode; items: AgentTaskHistoryItem[]; empty: string }) {
  return <section className="task-history-section"><div className="task-history-heading"><span>{icon}</span><strong>{title}</strong><small>{items.length}</small></div>{items.length ? <div className="task-history-list">{items.map(item => <TaskHistoryRow key={item.id} item={item} />)}</div> : <p className="task-history-empty">{empty}</p>}</section>;
}

function TaskHistoryRow({ item }: { item: AgentTaskHistoryItem }) {
  const tone = taskStatusTone(item.status);
  return <article className="task-history-row"><div className="task-history-row-head"><span className={`task-status-dot ${tone}`} /><strong>{taskTypeLabel(item.task_type)}</strong><span className={`task-status-pill ${tone}`}>{taskStatusLabel(item.status)}</span></div><div className="task-history-row-meta"><time>{formatTaskTime(item.finished_at || item.updated_at || item.created_at)}</time></div>{item.detail || item.error ? <p className={item.error ? 'error' : ''}>{item.error || item.detail}</p> : null}{['pending', 'running', 'canceling'].includes(item.status) ? <div className="task-progress"><span style={{ width: `${Math.max(0, Math.min(100, item.progress || 0))}%` }} /></div> : null}</article>;
}

function taskStatusLabel(status: string) { return ({ pending: '等待中', running: '运行中', canceling: '取消中', completed: '已完成', failed: '失败', canceled: '已取消' } as Record<string, string>)[status] || status || '未知'; }
function taskStatusTone(status: string) { if (status === 'completed') return 'success'; if (status === 'failed') return 'danger'; if (status === 'canceled') return 'neutral'; if (status === 'pending') return 'pending'; return 'running'; }
function formatTaskTime(value?: string | null) { if (!value) return '--'; const date = new Date(value); return Number.isNaN(date.getTime()) ? value : date.toLocaleString('zh-CN', { hour12: false }); }

function taskTypeLabel(taskType: string) {
  const labels: Record<string, string> = {
    upload_code: '代码上传',
    upload_placeholder: '准备文件上传',
    smb_upload: '共享目录上传',
    sync_exhibits: '项目同步',
    initialize_exhibit_repository: '项目初始化',
    agent_run: 'AI 任务',
  };
  return labels[taskType] || taskType || '远程任务';
}
