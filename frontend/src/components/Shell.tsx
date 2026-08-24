import { useCallback, useEffect, useRef, useState, type MouseEvent as ReactMouseEvent, type ReactNode } from 'react';
import { Blocks, BookOpen, Cable, CheckCircle2, ChevronDown, CircleAlert, CircleUserRound, ClipboardCheck, Clock3, ExternalLink, FileText, FolderOpen, Hammer, Info, LayoutDashboard, ListChecks, LoaderCircle, LogOut, MessageCircle, Minus, PanelLeftClose, PanelLeftOpen, RefreshCw, Settings, Square, X } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import type { PageKey } from '../types';
import type { AgentTaskHistoryItem, CurrentTaskStatus, DashboardIdentityStatus } from '../services/agentApi';

type ShellProps = {
  currentPage: PageKey;
  approvalCount: number;
  identity: DashboardIdentityStatus | null;
  dashboardEnabled: boolean;
  agentVersion: string;
  updateBusy: boolean;
  currentTask: CurrentTaskStatus | null;
  onLoadTaskHistory: () => Promise<AgentTaskHistoryItem[]>;
  onNavigate: (page: PageKey) => void;
  onOpenDashboard: () => void;
  onOpenBuiltinAi: () => void;
  onCheckUpdate: () => void;
  onOpenAgentDirectory: () => void;
  onQuit: () => void;
  children: ReactNode;
};

const navSections = [
  {
    label: 'Agent',
    items: [
      { key: 'dashboard', icon: LayoutDashboard, label: '概览' },
      { key: 'approvals', icon: ClipboardCheck, label: '审批' },
    ],
  },
  {
    label: '能力',
    items: [
      { key: 'ai', icon: Cable, label: 'AI 连接' },
      { key: 'skills', icon: BookOpen, label: '技能' },
      { key: 'plugins', icon: Blocks, label: '插件' },
    ],
  },
  {
    label: '设备',
    items: [
      { key: 'settings', icon: Settings, label: '设置' },
    ],
  },
] satisfies { label: string; items: { key: PageKey; icon: typeof LayoutDashboard; label: string }[] }[];

const developerNavItems = [
  { key: 'development', icon: Hammer, label: '扩展' },
  { key: 'logs', icon: FileText, label: '日志' },
] satisfies { key: PageKey; icon: typeof LayoutDashboard; label: string }[];

type MenuKey = 'agent' | 'view' | 'tools' | 'help';

function AppMenuBar({ currentPage, agentVersion, updateBusy, dashboardEnabled, onNavigate, onOpenDashboard, onOpenBuiltinAi, onCheckUpdate, onOpenAgentDirectory, onOpenTasks, onQuit }: Pick<ShellProps, 'currentPage' | 'agentVersion' | 'updateBusy' | 'dashboardEnabled' | 'onNavigate' | 'onOpenDashboard' | 'onOpenBuiltinAi' | 'onCheckUpdate' | 'onOpenAgentDirectory' | 'onQuit'> & { onOpenTasks: () => void }) {
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

  return (
    <>
      <div className="app-menu-bar" ref={menuBarRef} aria-label="应用菜单" data-tauri-drag-region onMouseDown={handleTitleBarMouseDown} onDoubleClick={handleTitleBarDoubleClick}>
        <div className="app-menu-brand" aria-label="HiMind Agent">
          <span className="app-menu-brand-mark"><span /></span>
          <strong>HiMind Agent</strong>
          <span className="app-menu-context">{pageLabel(currentPage)}</span>
        </div>
        <div className="app-menu-group">
          <button type="button" className={openMenu === 'agent' ? 'active' : ''} onClick={() => toggleMenu('agent')} aria-expanded={openMenu === 'agent'}>
            Agent <ChevronDown size={13} aria-hidden="true" />
          </button>
          {openMenu === 'agent' ? (
            <div className="app-menu-dropdown" role="menu">
              <button type="button" role="menuitem" onClick={() => runAction(onOpenBuiltinAi)}><MessageCircle size={16} /><span>打开 AI 对话</span></button>
              {dashboardEnabled ? <button type="button" role="menuitem" onClick={() => runAction(onOpenDashboard)}><ExternalLink size={16} /><span>打开工作台</span></button> : null}
              <div className="app-menu-separator" role="separator" />
              <button type="button" role="menuitem" className="danger" onClick={() => runAction(onQuit)}><LogOut size={16} /><span>退出 Agent</span></button>
            </div>
          ) : null}
        </div>

        <div className="app-menu-group">
          <button type="button" className={openMenu === 'view' ? 'active' : ''} onClick={() => toggleMenu('view')} aria-expanded={openMenu === 'view'}>
            查看 <ChevronDown size={13} aria-hidden="true" />
          </button>
          {openMenu === 'view' ? (
            <div className="app-menu-dropdown" role="menu">
              <button type="button" role="menuitem" onClick={() => runAction(() => onNavigate('dashboard'))}><LayoutDashboard size={16} /><span>概览</span></button>
              <button type="button" role="menuitem" onClick={() => runAction(() => onNavigate('approvals'))}><ClipboardCheck size={16} /><span>审批</span></button>
              <button type="button" role="menuitem" onClick={() => runAction(() => onNavigate('ai'))}><Cable size={16} /><span>AI 连接</span></button>
              <button type="button" role="menuitem" onClick={() => runAction(() => onNavigate('skills'))}><BookOpen size={16} /><span>技能</span></button>
              <button type="button" role="menuitem" onClick={() => runAction(() => onNavigate('plugins'))}><Blocks size={16} /><span>插件</span></button>
              <button type="button" role="menuitem" onClick={() => runAction(() => onNavigate('settings'))}><Settings size={16} /><span>设置</span></button>
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
              <button type="button" role="menuitem" onClick={() => runAction(() => onNavigate('logs'))}><FileText size={16} /><span>运行日志</span></button>
              <button type="button" role="menuitem" onClick={() => runAction(onOpenAgentDirectory)}><FolderOpen size={16} /><span>打开目录</span></button>
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
        <div className="app-window-controls" aria-label="窗口控制">
          <button type="button" className="app-window-control" title="最小化" aria-label="最小化" onClick={() => { void invoke('window_minimize').catch(error => console.error('窗口最小化失败', error)); }}><Minus size={14} /></button>
          <button type="button" className="app-window-control" title="最大化" aria-label="最大化" onClick={() => { void invoke('window_toggle_maximize').catch(error => console.error('窗口最大化失败', error)); }}><Square size={11} /></button>
          <button type="button" className="app-window-control close" title="关闭" aria-label="关闭" onClick={() => { void invoke('window_close').catch(error => console.error('窗口关闭失败', error)); }}><X size={14} /></button>
        </div>
      </div>

      {aboutOpen ? (
        <div className="modal-backdrop app-about-backdrop" role="presentation" onClick={event => { if (event.currentTarget === event.target) setAboutOpen(false); }}>
          <section className="modal app-about-modal" role="dialog" aria-modal="true" aria-labelledby="app-about-title">
            <button type="button" className="app-about-close" title="关闭" aria-label="关闭" onClick={() => setAboutOpen(false)}><X size={16} /></button>
            <img src="/brand/himind-app.png" alt="" aria-hidden="true" />
            <h2 id="app-about-title">HiMind Agent</h2>
            <span className="app-about-version">版本 {agentVersion}</span>
            <p>为员工数字分身提供本机执行能力。</p>
            <button type="button" className="btn btn-primary" onClick={() => setAboutOpen(false)}>确定</button>
          </section>
        </div>
      ) : null}
    </>
  );
}

function pageLabel(page: PageKey) {
  return ({ dashboard: '概览', 'builtin-ai': 'HiMind AI', approvals: '审批', ai: 'AI 连接', skills: '技能', plugins: '插件', development: '扩展', settings: '设置', logs: '日志' } as Record<PageKey, string>)[page];
}

export function Shell({ currentPage, approvalCount, identity, dashboardEnabled, agentVersion, updateBusy, currentTask, onLoadTaskHistory, onNavigate, onOpenDashboard, onOpenBuiltinAi, onCheckUpdate, onOpenAgentDirectory, onQuit, children }: ShellProps) {
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
      <AppMenuBar currentPage={currentPage} agentVersion={agentVersion} updateBusy={updateBusy} dashboardEnabled={dashboardEnabled} onNavigate={onNavigate} onOpenDashboard={onOpenDashboard} onOpenBuiltinAi={onOpenBuiltinAi} onCheckUpdate={onCheckUpdate} onOpenAgentDirectory={onOpenAgentDirectory} onOpenTasks={() => setTaskDrawerOpen(true)} onQuit={onQuit} />
      <div className="shell-body">
        <aside className={`sidebar${sidebarCollapsed ? ' collapsed' : ''}`}>
        <div className="sidebar-header">
          <img className="product-mark" src="/brand/himind-app.png" alt="" aria-hidden="true" />
          <div>
            <h1>HiMind</h1>
            <div className="product-type">数字分身</div>
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
        <nav aria-label="主导航">
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
          {navSections.map(section => (
            <div className="sidebar-nav-group" key={section.label}>
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
                  {item.key === 'approvals' && approvalCount > 0 ? <span className="badge">{approvalCount}</span> : null}
                </button>
              ))}
            </div>
          ))}
          {dashboardEnabled ? <div className="sidebar-developer">
            <span className="sidebar-section-label">开发</span>
            {developerNavItems.map(item => (
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
              </button>
            ))}
          </div> : null}
        </nav>
        {dashboardEnabled ? <button className={`sidebar-account ${identity?.authorized ? 'authorized' : ''}`} type="button" onClick={() => { onNavigate('dashboard'); window.setTimeout(() => document.getElementById('account-authorization')?.scrollIntoView({ behavior: 'smooth', block: 'start' }), 0); }} title="HiMind 账号">
          <CircleUserRound size={18} />
          <span><strong>{identity?.authorized ? identity.user_name || 'HiMind 账号' : 'HiMind 账号未连接'}</strong><small>{identity?.authorized ? '账号已连接' : '连接 HiMind 账号'}</small></span>
          <span className={`status-dot sidebar-account-status ${identity?.authorized ? 'success' : ''}`} />
        </button> : <div className="sidebar-account independent-account" title="本机服务状态">
          <CheckCircle2 size={18} />
          <span><strong>独立运行</strong><small>本机服务已就绪</small></span>
          <span className="status-dot sidebar-account-status success" />
        </div>}
        </aside>
        <main className={`main${currentPage === 'builtin-ai' ? ' builtin-ai-main' : ''}`}>
          {dashboardEnabled && currentTask && currentPage !== 'builtin-ai' ? <button type="button" className="current-task-strip" onClick={() => setTaskDrawerOpen(true)} title="查看当前任务"><LoaderCircle size={15} className="spin" /><span><strong>正在执行 {taskTypeLabel(currentTask.task_type)}</strong><small>{currentTask.task_id}</small></span><code>{currentTask.execution_id || '本机执行'}</code><span className="current-task-open-label">任务记录</span></button> : null}
          {children}
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
      <header className="task-drawer-header"><div><span className="task-drawer-kicker">Agent 工作记录</span><h2 id="task-drawer-title">任务记录</h2></div><div className="task-drawer-actions"><button type="button" className="btn btn-icon" title="刷新任务记录" aria-label="刷新任务记录" onClick={onRefresh} disabled={loading}><RefreshCw size={16} className={loading ? 'spin' : ''} /></button><button type="button" className="btn btn-icon" title="关闭任务记录" aria-label="关闭任务记录" onClick={onClose}><X size={17} /></button></div></header>
      {currentTask ? <section className="task-current-summary"><div className="task-summary-icon"><LoaderCircle size={17} className="spin" /></div><div><strong>正在执行 · {taskTypeLabel(currentTask.task_type)}</strong><small>{currentTask.task_id}</small></div><span className="task-status-pill running">运行中</span></section> : null}
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
  return <article className="task-history-row"><div className="task-history-row-head"><span className={`task-status-dot ${tone}`} /><strong>{taskTypeLabel(item.task_type)}</strong><span className={`task-status-pill ${tone}`}>{taskStatusLabel(item.status)}</span></div><div className="task-history-row-meta"><code>{item.id}</code><time>{formatTaskTime(item.finished_at || item.updated_at || item.created_at)}</time></div>{item.detail || item.error ? <p className={item.error ? 'error' : ''}>{item.error || item.detail}</p> : null}{['pending', 'running', 'canceling'].includes(item.status) ? <div className="task-progress"><span style={{ width: `${Math.max(0, Math.min(100, item.progress || 0))}%` }} /></div> : null}</article>;
}

function taskStatusLabel(status: string) { return ({ pending: '等待中', running: '运行中', canceling: '取消中', completed: '已完成', failed: '失败', canceled: '已取消' } as Record<string, string>)[status] || status || '未知'; }
function taskStatusTone(status: string) { if (status === 'completed') return 'success'; if (status === 'failed') return 'danger'; if (status === 'canceled') return 'neutral'; if (status === 'pending') return 'pending'; return 'running'; }
function formatTaskTime(value?: string | null) { if (!value) return '--'; const date = new Date(value); return Number.isNaN(date.getTime()) ? value : date.toLocaleString('zh-CN', { hour12: false }); }

function taskTypeLabel(taskType: string) {
  const labels: Record<string, string> = {
    upload_code: '代码上传',
    upload_placeholder: '占位上传',
    smb_upload: '共享目录上传',
    sync_exhibits: '展项同步',
    initialize_exhibit_repository: '展项初始化',
    agent_run: 'AI 执行',
  };
  return labels[taskType] || taskType || '远程任务';
}
