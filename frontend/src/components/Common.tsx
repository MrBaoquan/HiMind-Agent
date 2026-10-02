import { CircleAlert, CircleCheck, Info, X, type LucideIcon } from 'lucide-react';
import type { ReactNode } from 'react';
import type { UiMessage } from '../types';

const notificationIcons = {
  success: CircleCheck,
  error: CircleAlert,
  info: Info,
};

export function NotificationCenter({ messages, onClose }: { messages: UiMessage[]; onClose: (id: number) => void }) {
  if (!messages.length) return null;
  return (
    <div className="notification-region">
      {messages.map(message => {
        const Icon = notificationIcons[message.kind];
        return (
          <div key={message.id} className={`app-notification ${message.kind}`} role={message.kind === 'error' ? 'alert' : 'status'} aria-live={message.kind === 'error' ? 'assertive' : 'polite'}>
            <div className="notification-icon"><Icon size={17} aria-hidden="true" /></div>
            <div className="notification-content">
              <strong>{message.kind === 'success' ? '操作成功' : message.kind === 'error' ? '操作失败' : '提示'}</strong>
              <span>{message.text}</span>
            </div>
            <button type="button" className="notification-close" title="关闭通知" aria-label="关闭通知" onClick={() => onClose(message.id)}><X size={15} /></button>
          </div>
        );
      })}
    </div>
  );
}

/**
 * 页头只承担定位，不承担说明。
 * 标签本身已经说清的事不再重复；`description` 省略时整行不渲染，
 * 避免出现「为了填满组件而写一句话」的填充文案。
 */
export function PageHeader({ title, description, actions }: { title: string; description?: string; actions?: ReactNode }) {
  return (
    <header className={`page-header${actions ? ' has-actions' : ''}`}>
      <div>
        <h2>{title}</h2>
        {description ? <p>{description}</p> : null}
      </div>
      {actions ? <div className="page-actions">{actions}</div> : null}
    </header>
  );
}

export function IconButton({ icon: Icon, label, onClick, disabled }: { icon: LucideIcon; label: string; onClick: () => void; disabled?: boolean }) {
  return <button type="button" className="btn btn-icon" title={label} aria-label={label} onClick={onClick} disabled={disabled}><Icon size={16} /></button>;
}

/** 空态承载「为什么空 + 下一步」；`text` 省略时只留标题，不写凑数的解释。 */
export function EmptyState({ icon: Icon, title, text }: { icon: LucideIcon; title: string; text?: string }) {
  return (
    <div className="empty">
      <div className="empty-icon"><Icon size={20} aria-hidden="true" /></div>
      <strong>{title}</strong>
      {text ? <span>{text}</span> : null}
    </div>
  );
}

/** live 表示「此刻正在跑」，其余 kind 表示已落定的结果。 */
export function Pill({ kind, children }: { kind: 'success' | 'warn' | 'danger' | 'neutral' | 'live'; children: ReactNode }) {
  return <span className={`pill ${kind}`}>{children}</span>;
}

export function Tags({ items }: { items?: string[] }) {
  if (!items?.length) return <span className="muted">--</span>;
  return (
    <div className="tag-list">
      {items.map(item => <span className="tag" key={item}>{item}</span>)}
    </div>
  );
}
