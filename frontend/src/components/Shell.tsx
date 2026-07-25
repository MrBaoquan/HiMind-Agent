import type { ReactNode } from 'react';
import { Blocks, BookOpen, Cable, CircleUserRound, ClipboardCheck, FileText, LayoutDashboard, Settings } from 'lucide-react';
import type { PageKey } from '../types';
import type { DashboardIdentityStatus } from '../services/agentApi';

type ShellProps = {
  currentPage: PageKey;
  approvalCount: number;
  identity: DashboardIdentityStatus | null;
  onNavigate: (page: PageKey) => void;
  children: ReactNode;
};

const navItems = [
  { key: 'dashboard', icon: LayoutDashboard, label: '总览' },
  { key: 'ai', icon: Cable, label: '连接 AI' },
  { key: 'approvals', icon: ClipboardCheck, label: '审批' },
  { key: 'plugins', icon: Blocks, label: '插件' },
  { key: 'skills', icon: BookOpen, label: '技能' },
  { key: 'logs', icon: FileText, label: '日志' },
  { key: 'settings', icon: Settings, label: '设置' },
] satisfies { key: PageKey; icon: typeof LayoutDashboard; label: string }[];

export function Shell({ currentPage, approvalCount, identity, onNavigate, children }: ShellProps) {
  return (
    <div className="shell">
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
        </nav>
        <button className={`sidebar-account ${identity?.authorized ? 'authorized' : ''}`} type="button" onClick={() => { onNavigate('dashboard'); window.setTimeout(() => document.getElementById('account-authorization')?.scrollIntoView({ behavior: 'smooth', block: 'start' }), 0); }} title="HiMind 账号">
          <CircleUserRound size={18} />
          <span><strong>{identity?.authorized ? identity.user_name || '已授权账号' : '登录 HiMind'}</strong><small>{identity?.authorized ? '账号已授权' : '使用工作台能力'}</small></span>
          <span className={`status-dot ${identity?.authorized ? 'success' : ''}`} />
        </button>
      </aside>
      <main className="main">{children}</main>
    </div>
  );
}
