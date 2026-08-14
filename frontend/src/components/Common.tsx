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

export function PageHeader({ title, description, actions }: { title: string; description: string; actions?: ReactNode }) {
  return (
    <header className={`page-header${actions ? ' has-actions' : ''}`}>
      <div>
        <h2>{title}</h2>
        <p>{description}</p>
      </div>
      {actions ? <div className="page-actions">{actions}</div> : null}
    </header>
  );
}

export function IconButton({ icon: Icon, label, onClick, disabled }: { icon: LucideIcon; label: string; onClick: () => void; disabled?: boolean }) {
  return <button type="button" className="btn btn-icon" title={label} aria-label={label} onClick={onClick} disabled={disabled}><Icon size={16} /></button>;
}

export function EmptyState({ icon: Icon, title, text }: { icon: LucideIcon; title: string; text: string }) {
  return (
    <div className="empty">
      <div className="empty-icon"><Icon size={20} aria-hidden="true" /></div>
      <strong>{title}</strong>
      <span>{text}</span>
    </div>
  );
}

export function Pill({ kind, children }: { kind: 'success' | 'warn' | 'danger' | 'neutral'; children: ReactNode }) {
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
