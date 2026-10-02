import { LoaderCircle } from 'lucide-react';

type BusyIndicatorProps = {
  size?: number;
  className?: string;
  /** 传了才播报（role="status"）；旁边已有「正在…」文案时不要传，免得读屏念两遍。 */
  label?: string;
};

/**
 * 全应用唯一的「正在处理」图形。
 *
 * 加载语义只用 LoaderCircle：刷新图标（RefreshCw）表示「这里可以刷新」这个动作，
 * 不再兼职加载指示 —— 同一个图标两种含义，用户分不清点下去是重取还是刷新。
 *
 * 它按固定速度常转，不受系统「减少动态效果」影响：静止的转圈和「卡死」长得一样，
 * 用户只能靠猜，而旋转本身能上合成线程、成本可忽略（实测见 styles.css 末尾）。
 * 旁边照旧要有「正在…」文案，图形不单独承担语义。
 */
export function BusyIndicator({ size = 15, className, label }: BusyIndicatorProps) {
  return (
    <LoaderCircle
      className={className ? `busy-indicator ${className}` : 'busy-indicator'}
      size={size}
      role={label ? 'status' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
    />
  );
}
