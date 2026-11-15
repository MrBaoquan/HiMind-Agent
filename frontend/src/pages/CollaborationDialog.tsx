import { useEffect, useRef, type ReactNode } from 'react';
import { createPortal } from 'react-dom';

/** Native modal semantics keep background inert, trap Tab and restore focus. */
export function CollaborationDialog({ titleId, onClose, children }: { titleId: string; onClose: () => void; children: ReactNode }) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = ref.current;
    const trigger = document.activeElement as HTMLElement | null;
    dialog?.showModal();
    return () => { dialog?.close(); if (trigger?.isConnected) trigger.focus(); };
  }, []);
  return createPortal(<dialog ref={ref} className="collaboration-modal" aria-labelledby={titleId} onCancel={event => { event.preventDefault(); onClose(); }}>
    {children}
  </dialog>, document.body);
}
