import { useEffect, useRef, useState, type ReactNode } from 'react';
import { Blocks, BookOpen, Cable, ChevronDown, CircleUserRound, ClipboardCheck, ExternalLink, FileText, FolderOpen, Hammer, Info, LayoutDashboard, LogOut, RefreshCw, Settings, X } from 'lucide-react';
import type { PageKey } from '../types';
import type { DashboardIdentityStatus } from '../services/agentApi';

type ShellProps = {
  currentPage: PageKey;
  approvalCount: number;
  identity: DashboardIdentityStatus | null;
  agentVersion: string;
  updateBusy: boolean;
  onNavigate: (page: PageKey) => void;
  onOpenDashboard: () => void;
  onCheckUpdate: () => void;
  onOpenAgentDirectory: () => void;
  onQuit: () => void;
  children: ReactNode;
};

const navItems = [
  { key: 'dashboard', icon: LayoutDashboard, label: '总览' },
  { key: 'ai', icon: Cable, label: 'AI 工具连接' },
  { key: 'approvals', icon: ClipboardCheck, label: '审批' },
  { key: 'plugins', icon: Blocks, label: '插件' },
  { key: 'skills', icon: BookOpen, label: '技能' },
  { key: 'settings', icon: Settings, label: '设置' },
] satisfies { key: PageKey; icon: typeof LayoutDashboard; label: string }[];

const developerNavItems = [
  { key: 'development', icon: Hammer, label: '扩展开发' },
  { key: 'logs', icon: FileText, label: '诊断日志' },
] satisfies { key: PageKey; icon: typeof LayoutDashboard; label: string }[];

type MenuKey = 'application' | 'diagnostics' | 'help';

function AppMenuBar({ agentVersion, updateBusy, onNavigate, onOpenDashboard, onCheckUpdate, onOpenAgentDirectory, onQuit }: Pick<ShellProps, 'agentVersion' | 'updateBusy' | 'onNavigate' | 'onOpenDashboard' | 'onCheckUpdate' | 'onOpenAgentDirectory' | 'onQuit'>) {
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

  return (
    <>
      <div className="app-menu-bar" ref={menuBarRef} aria-label="应用菜单">
        <div className="app-menu-group">
          <button type="button" className={openMenu === 'application' ? 'active' : ''} onClick={() => toggleMenu('application')} aria-expanded={openMenu === 'application'}>
            应用 <ChevronDown size={13} aria-hidden="true" />
          </button>
          {openMenu === 'application' ? (
            <div className="app-menu-dropdown" role="menu">
              <button type="button" role="menuitem" onClick={() => runAction(onOpenDashboard)}><ExternalLink size={16} /><span>打开工作台</span></button>
              <button type="button" role="menuitem" disabled={updateBusy} onClick={() => runAction(onCheckUpdate)}><RefreshCw className={updateBusy ? 'spin' : ''} size={16} /><span>{updateBusy ? '正在检查更新' : '检查更新'}</span></button>
              <div className="app-menu-separator" role="separator" />
              <button type="button" role="menuitem" className="danger" onClick={() => runAction(onQuit)}><LogOut size={16} /><span>退出 HiMind Agent</span></button>
            </div>
          ) : null}
        </div>

        <div className="app-menu-group">
          <button type="button" className={openMenu === 'diagnostics' ? 'active' : ''} onClick={() => toggleMenu('diagnostics')} aria-expanded={openMenu === 'diagnostics'}>
            诊断 <ChevronDown size={13} aria-hidden="true" />
          </button>
          {openMenu === 'diagnostics' ? (
            <div className="app-menu-dropdown" role="menu">
              <button type="button" role="menuitem" onClick={() => runAction(() => onNavigate('logs'))}><FileText size={16} /><span>诊断日志</span></button>
              <button type="button" role="menuitem" onClick={() => runAction(onOpenAgentDirectory)}><FolderOpen size={16} /><span>打开 Agent 文件夹</span></button>
            </div>
          ) : null}
        </div>

        <div className="app-menu-group">
          <button type="button" className={openMenu === 'help' ? 'active' : ''} onClick={() => toggleMenu('help')} aria-expanded={openMenu === 'help'}>
            帮助 <ChevronDown size={13} aria-hidden="true" />
          </button>
          {openMenu === 'help' ? (
            <div className="app-menu-dropdown" role="menu">
              <button type="button" role="menuitem" onClick={() => { setOpenMenu(null); setAboutOpen(true); }}><Info size={16} /><span>关于 HiMind Agent</span></button>
            </div>
          ) : null}
        </div>
      </div>

      {aboutOpen ? (
        <div className="modal-backdrop app-about-backdrop" role="presentation" onClick={event => { if (event.currentTarget === event.target) setAboutOpen(false); }}>
          <section className="modal app-about-modal" role="dialog" aria-modal="true" aria-labelledby="app-about-title">
            <button type="button" className="app-about-close" title="关闭" aria-label="关闭" onClick={() => setAboutOpen(false)}><X size={16} /></button>
            <img src="/brand/himind-app.png" alt="" aria-hidden="true" />
            <h2 id="app-about-title">HiMind Agent</h2>
            <span className="app-about-version">版本 {agentVersion}</span>
            <p>连接 HiMind 工作台与本机 AI 工具。</p>
            <button type="button" className="btn btn-primary" onClick={() => setAboutOpen(false)}>确定</button>
          </section>
        </div>
      ) : null}
    </>
  );
}

export function Shell({ currentPage, approvalCount, identity, agentVersion, updateBusy, onNavigate, onOpenDashboard, onCheckUpdate, onOpenAgentDirectory, onQuit, children }: ShellProps) {
  return (
    <div className="shell">
      <AppMenuBar agentVersion={agentVersion} updateBusy={updateBusy} onNavigate={onNavigate} onOpenDashboard={onOpenDashboard} onCheckUpdate={onCheckUpdate} onOpenAgentDirectory={onOpenAgentDirectory} onQuit={onQuit} />
      <div className="shell-body">
        <aside className="sidebar">
        <div className="sidebar-header">
          <img className="product-mark" src="/brand/himind-app.png" alt="" aria-hidden="true" />
          <div>
            <h1>HiMind</h1>
            <div className="product-type">本机 Agent</div>
          </div>
        </div>
        <nav aria-label="主导航">
          {navItems.map(item => (
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
          <div className="sidebar-developer">
            <span className="sidebar-section-label">开发者</span>
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
          </div>
        </nav>
        <button className={`sidebar-account ${identity?.authorized ? 'authorized' : ''}`} type="button" onClick={() => { onNavigate('dashboard'); window.setTimeout(() => document.getElementById('account-authorization')?.scrollIntoView({ behavior: 'smooth', block: 'start' }), 0); }} title="HiMind 账号">
          <CircleUserRound size={18} />
          <span><strong>{identity?.authorized ? identity.user_name || '已登录 HiMind' : '登录 HiMind'}</strong><small>{identity?.authorized ? '账号正常' : '使用工作台功能'}</small></span>
          <span className={`status-dot ${identity?.authorized ? 'success' : ''}`} />
        </button>
        </aside>
        <main className="main">{children}</main>
      </div>
    </div>
  );
}
