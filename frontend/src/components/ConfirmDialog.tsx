import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { X } from 'lucide-react';

/**
 * 破坏性操作的统一确认弹窗。
 *
 * 桌面端调用 window.confirm 弹出的是 WebView 自带的系统框（标题是 tauri.localhost，
 * 按钮是「确定 / 取消」），和页面里卸载、停用那套 .modal 不是一套视觉语言——同一件
 * 「确认一下」在应用里长两个样子。这里把确认框收回应用内，只用一套样式。
 */
export type ConfirmOptions = {
  /** 标题就是这次要做的动作，用问句。 */
  title: string;
  /** 后果说明：一句话讲清「点了会发生什么 / 不会发生什么」。 */
  description?: string;
  confirmText?: string;
  cancelText?: string;
  /** 默认按危险操作展示；只有确实不可逆时才需要显式写 true。 */
  danger?: boolean;
};

type ConfirmFn = (options: ConfirmOptions) => Promise<boolean>;

const ConfirmContext = createContext<ConfirmFn | null>(null);

export function useConfirm(): ConfirmFn {
  const confirm = useContext(ConfirmContext);
  if (!confirm) throw new Error('useConfirm 只能在 ConfirmProvider 内部使用');
  return confirm;
}

type PendingConfirm = { id: number; options: ConfirmOptions; resolve: (accepted: boolean) => void };

export function ConfirmProvider({ children }: { children: ReactNode }) {
  const [pending, setPending] = useState<PendingConfirm | null>(null);
  // 用 ref 持有 resolver：结算动作发生在事件回调里，不需要靠 state 更新副作用去 resolve。
  const pendingRef = useRef<PendingConfirm | null>(null);
  const nextId = useRef(0);

  const request = useCallback<ConfirmFn>(options => new Promise<boolean>(resolve => {
    // 上一张还没结算就再来一张：前一张按「取消」收尾，避免它的 await 永远挂着。
    pendingRef.current?.resolve(false);
    const next = { id: (nextId.current += 1), options, resolve };
    pendingRef.current = next;
    setPending(next);
  }), []);

  const settle = useCallback((accepted: boolean) => {
    const current = pendingRef.current;
    pendingRef.current = null;
    setPending(null);
    current?.resolve(accepted);
  }, []);

  // 确认框是模态的：Esc 只关它，不再往下传，免得顺手关掉底下的抽屉或页面弹窗。
  useEffect(() => {
    if (!pending) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      event.preventDefault();
      event.stopPropagation();
      settle(false);
    };
    document.addEventListener('keydown', onKeyDown, true);
    return () => document.removeEventListener('keydown', onKeyDown, true);
  }, [pending, settle]);

  const value = useMemo(() => request, [request]);
  const titleId = pending ? `confirm-dialog-title-${pending.id}` : undefined;

  return (
    <ConfirmContext.Provider value={value}>
      {children}
      {pending ? (
        <div
          className="modal-backdrop is-confirm"
          role="presentation"
          onClick={event => { if (event.currentTarget === event.target) settle(false); }}
        >
          <section className="modal confirm-modal" role="dialog" aria-modal="true" aria-labelledby={titleId}>
            <div className="modal-header">
              <div>
                <h3 id={titleId}>{pending.options.title}</h3>
                {pending.options.description ? <p>{pending.options.description}</p> : null}
              </div>
              <button type="button" className="btn btn-icon" title="关闭" aria-label="关闭" onClick={() => settle(false)}><X size={16} /></button>
            </div>
            <div className="modal-body">
              <div className="modal-actions">
                {/* 焦点默认落在取消上：破坏性动作不该被一次回车确认掉。 */}
                <button type="button" className="btn" autoFocus onClick={() => settle(false)}>{pending.options.cancelText || '取消'}</button>
                <button
                  type="button"
                  className={pending.options.danger === false ? 'btn btn-primary' : 'btn btn-danger'}
                  onClick={() => settle(true)}
                >
                  {pending.options.confirmText || '确认'}
                </button>
              </div>
            </div>
          </section>
        </div>
      ) : null}
    </ConfirmContext.Provider>
  );
}
