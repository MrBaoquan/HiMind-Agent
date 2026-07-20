import type { ReactNode } from 'react';
import { Blocks, BookOpen, ClipboardCheck, FileText, LayoutDashboard, Settings } from 'lucide-react';
import type { PageKey } from '../types';

type ShellProps = {
  version?: string;
  currentPage: PageKey;
  approvalCount: number;
  onNavigate: (page: PageKey) => void;
  children: ReactNode;
};

const navItems = [
  { key: 'dashboard', icon: LayoutDashboard, label: '总览' },
  { key: 'approvals', icon: ClipboardCheck, label: '审批' },
  { key: 'plugins', icon: Blocks, label: '插件' },
  { key: 'skills', icon: BookOpen, label: '技能' },
  { key: 'logs', icon: FileText, label: '日志' },
  { key: 'settings', icon: Settings, label: '设置' },
] satisfies { key: PageKey; icon: typeof LayoutDashboard; label: string }[];

export function Shell({ version, currentPage, approvalCount, onNavigate, children }: ShellProps) {
  return (
    <div className="shell">
      <aside className="sidebar">
        <div className="sidebar-header">
          <div className="product-mark" aria-hidden="true">H</div>
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
            >
              <item.icon size={17} strokeWidth={1.8} aria-hidden="true" />
              <span>{item.label}</span>
              {item.key === 'approvals' && approvalCount > 0 ? <span className="badge">{approvalCount}</span> : null}
            </button>
          ))}
        </nav>
        <div className="sidebar-footer">
          <span className="status-dot success" aria-hidden="true" />
          <span>本地服务</span>
          <span className="version">v{version || '0.2.0'}</span>
        </div>
      </aside>
      <main className="main">{children}</main>
    </div>
  );
}
