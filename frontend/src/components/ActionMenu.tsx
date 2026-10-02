import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import { ChevronDown } from 'lucide-react';

// 全应用唯一的聚合入口。设计规则：
//   · 一屏只出现一个（页面级、详情级、卡片级）→ 带文字 + 下拉箭头，让人看得见里面装了什么；
//   · 一屏可能出现多个（列表行）→ 只留图标，避免重复标签把列表压花。
// 行为上补齐原生 <details> 缺的三件事：点外部关闭、Esc 关闭（焦点回到触发器）、选中后关闭；
// 并保证同一时刻只开一个。面板用 fixed 定位，按触发器位置计算，放不下自动翻到上方。
const PANEL_GAP = 6;
const PANEL_MARGIN = 8;

let activeMenuCloser: (() => void) | null = null;

export function ActionMenu({ label, icon, title, variant = 'default', disabled = false, className, panelClassName = 'app-menu-dropdown', panelWidth, align = 'end', onOpen, children }: {
  /** 可见文字。给了就是"页面级/详情级"入口；不给则渲染成纯图标按钮 */
  label?: string;
  icon: ReactNode;
  title?: string;
  variant?: 'default' | 'primary' | 'icon';
  disabled?: boolean;
  className?: string;
  panelClassName?: string;
  /** 用于定位估算的面板宽度；缺省按菜单皮肤 208px */
  panelWidth?: number;
  align?: 'start' | 'end';
  onOpen?: () => void;
  children: (close: () => void) => ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState<{ top: number; left: number } | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const panelRef = useRef<HTMLDivElement | null>(null);

  const close = useCallback(() => {
    setOpen(false);
    setPosition(null);
    if (activeMenuCloser === close) activeMenuCloser = null;
  }, []);

  const place = useCallback(() => {
    const rect = triggerRef.current?.getBoundingClientRect();
    if (!rect) return;
    const width = panelRef.current?.offsetWidth || panelWidth || 208;
    const height = panelRef.current?.offsetHeight || 0;
    const preferred = align === 'end' ? rect.right - width : rect.left;
    const left = Math.max(PANEL_MARGIN, Math.min(preferred, window.innerWidth - width - PANEL_MARGIN));
    const below = rect.bottom + PANEL_GAP;
    const top = height && below + height > window.innerHeight - PANEL_MARGIN
      ? Math.max(PANEL_MARGIN, rect.top - PANEL_GAP - height)
      : below;
    setPosition({ top, left });
  }, [align, panelWidth]);

  // 先渲染再测量，测量发生在绘制前，所以不会闪一下错位。
  useLayoutEffect(() => {
    if (open) place();
  }, [open, place]);

  useEffect(() => {
    if (!open) return;
    const handlePointerDown = (event: PointerEvent) => {
      if (!triggerRef.current?.contains(event.target as Node) && !panelRef.current?.contains(event.target as Node)) close();
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      event.stopPropagation();
      close();
      triggerRef.current?.focus();
    };
    const handleLayoutChange = () => place();
    document.addEventListener('pointerdown', handlePointerDown, true);
    document.addEventListener('keydown', handleKeyDown, true);
    window.addEventListener('resize', handleLayoutChange);
    window.addEventListener('scroll', handleLayoutChange, true);
    return () => {
      document.removeEventListener('pointerdown', handlePointerDown, true);
      document.removeEventListener('keydown', handleKeyDown, true);
      window.removeEventListener('resize', handleLayoutChange);
      window.removeEventListener('scroll', handleLayoutChange, true);
    };
  }, [open, close, place]);

  useEffect(() => () => {
    if (activeMenuCloser === close) activeMenuCloser = null;
  }, [close]);

  const toggle = () => {
    if (disabled) return;
    if (open) {
      close();
      return;
    }
    if (activeMenuCloser && activeMenuCloser !== close) activeMenuCloser();
    activeMenuCloser = close;
    onOpen?.();
    setOpen(true);
  };

  const triggerClass = variant === 'primary'
    ? 'btn btn-primary'
    : variant === 'icon' || !label
      ? `btn btn-icon${open ? ' is-open' : ''}`
      : 'btn';

  return (
    <div className={['action-menu', className].filter(Boolean).join(' ')}>
      <button
        type="button"
        ref={triggerRef}
        className={triggerClass}
        title={title || label}
        aria-label={title || label}
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={disabled}
        onClick={toggle}
      >
        {icon}
        {label ? <span>{label}</span> : null}
        {label ? <ChevronDown size={13} aria-hidden="true" /> : null}
      </button>
      {open ? (
        <div
          ref={panelRef}
          className={['action-menu-panel', panelClassName].filter(Boolean).join(' ')}
          role="menu"
          style={position ? { top: position.top, left: position.left } : { top: -9999, left: -9999, visibility: 'hidden' }}
        >
          {children(close)}
        </div>
      ) : null}
    </div>
  );
}

/** 与标题栏菜单一致的菜单行：图标 + 文字（+ 右侧状态/徽标）。 */
export function ActionMenuItem({ icon, label, title, state, badge, danger = false, disabled = false, onClick }: {
  icon: ReactNode;
  label: string;
  title?: string;
  state?: string;
  badge?: ReactNode;
  danger?: boolean;
  disabled?: boolean;
  onClick: () => void;
}) {
  return (
    <button type="button" role="menuitem" className={danger ? 'danger' : undefined} title={title} disabled={disabled} onClick={onClick}>
      {icon}
      <span>{label}</span>
      {state ? <small className="extension-source-menu-state">{state}</small> : null}
      {badge}
    </button>
  );
}
