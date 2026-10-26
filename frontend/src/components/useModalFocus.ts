import { useEffect, useRef, type RefObject } from 'react';

const FOCUSABLE_SELECTOR = [
  'button:not([disabled])',
  'a[href]',
  'input:not([disabled]):not([type="hidden"])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  '[tabindex]:not([tabindex="-1"])',
].join(',');

function focusableItems(dialog: HTMLElement) {
  return Array.from(dialog.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR))
    .filter(item => item.getClientRects().length > 0 && item.getAttribute('aria-hidden') !== 'true');
}

/**
 * 让模态框真正接管键盘：进入后聚焦、Tab 循环、Esc 关闭、关闭后回到触发位置。
 * 背景隔离依赖 aria-modal；这里只补齐浏览器不会自动提供的键盘行为。
 */
export function useModalFocus(
  dialogRef: RefObject<HTMLElement | null>,
  active: boolean,
  onClose: () => void,
  initialFocusRef?: RefObject<HTMLElement | null>,
  returnFocusRef?: RefObject<HTMLElement | null>,
) {
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  useEffect(() => {
    if (!active) return;
    const dialog = dialogRef.current;
    if (!dialog) return;
    const previous = returnFocusRef?.current || (document.activeElement instanceof HTMLElement ? document.activeElement : null);
    const initial = initialFocusRef?.current || focusableItems(dialog)[0] || dialog;
    initial.focus({ preventScroll: true });

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        event.stopPropagation();
        onCloseRef.current();
        return;
      }
      if (event.key !== 'Tab') return;
      const items = focusableItems(dialog);
      if (!items.length) {
        event.preventDefault();
        dialog.focus({ preventScroll: true });
        return;
      }
      const first = items[0];
      const last = items[items.length - 1];
      const current = document.activeElement;
      if (!dialog.contains(current)) {
        event.preventDefault();
        (event.shiftKey ? last : first).focus();
        return;
      }
      if (event.shiftKey && current === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && current === last) {
        event.preventDefault();
        first.focus();
      }
    };

    document.addEventListener('keydown', onKeyDown, true);
    return () => {
      document.removeEventListener('keydown', onKeyDown, true);
      if (previous?.isConnected) previous.focus({ preventScroll: true });
    };
  }, [active, dialogRef, initialFocusRef, returnFocusRef]);
}
