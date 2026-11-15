import { useLayoutEffect, useRef, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import './ModalSurface.css';

/** Native top layer provides inert background, nested dialogs and keyboard containment. */
export function ModalSurface({ children, className, titleId, onClose, fallbackFocus }: {
  children: ReactNode;
  className: string;
  titleId: string;
  onClose: () => void;
  fallbackFocus?: () => HTMLElement | null;
}) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const closeRef = useRef(onClose);
  const fallbackRef = useRef(fallbackFocus);
  closeRef.current = onClose;
  fallbackRef.current = fallbackFocus;

  useLayoutEffect(() => {
    const dialog = dialogRef.current!;
    const trigger = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    dialog.showModal();
    dialog.querySelector<HTMLElement>('[data-initial-focus]')?.focus({ preventScroll: true });
    return () => {
      dialog.close();
      // React removes the dialog before re-enabling the trigger in the same commit.
      queueMicrotask(() => {
        const target = trigger?.isConnected && !trigger.matches(':disabled, [inert]') ? trigger : fallbackRef.current?.();
        const modals = document.querySelectorAll('dialog:modal');
        const topModal = modals[modals.length - 1];
        if (target && (!topModal || topModal.contains(target))) target.focus({ preventScroll: true });
      });
    };
  }, []);

  return createPortal(
    <dialog ref={dialogRef} className={`modal-surface ${className}`} aria-labelledby={titleId} aria-modal="true"
      onCancel={event => { event.preventDefault(); closeRef.current(); }}
      onKeyDown={event => {
        if (event.key === 'Escape') { event.stopPropagation(); return; }
        if (event.key !== 'Tab') return;
        event.stopPropagation();
        const items = [...event.currentTarget.querySelectorAll<HTMLElement>('button:not(:disabled),input:not(:disabled),select:not(:disabled),textarea:not(:disabled),a[href],[tabindex]:not([tabindex="-1"])')]
          .filter(item => item.getClientRects().length > 0 && !item.closest('[inert]'));
        const first = items[0];
        const last = items[items.length - 1];
        if (!first) { event.preventDefault(); return; }
        if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
        else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
      }}>
      {children}
    </dialog>, document.body,
  );
}
