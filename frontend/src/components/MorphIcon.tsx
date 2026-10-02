import type { IconNode } from 'lucide';
import { MorphIcon as MorphIconBase, type IconInput } from 'morphicons/react';

type MorphIconProps = {
  /** `lucide` 数据包里的图标数据（不是 lucide-react 组件），换了图标位就自动形变。 */
  icon: IconNode;
  size?: number;
  strokeWidth?: number;
  className?: string;
  color?: string;
  /** 传了才进无障碍树；不传按装饰性图标处理，避免和旁边的状态文字重复播报。 */
  label?: string;
};

/**
 * 图标形变的项目级封装。
 *
 * 只解决一件事：**同一个图标位从一个含义变成另一个含义**（等待 → 运行 → 完成）。
 * 持续运行态交给 BusyIndicator —— 形变转完就停，它表达不了「还在跑」。
 *
 * 两个默认值是有意收口的：`reducedMotion="never"` 明确让形变始终播放 ——
 * 形变是「状态变了」这件事本身的表达，系统的「减少动态效果」一旦介入，
 * 同一套界面在不同机器上就有两种形态（详见 styles.css 末尾的取舍说明）；
 * `strokeWidth` 与项目里 lucide-react 的用法对齐，免得同一行里形变图标和静态图标粗细不一致。
 *
 * 形变的是同一个 `<path>` 的 `d`，所以旋转（BusyIndicator）和形变互不干扰。
 */
export function MorphIcon({ icon, size = 16, strokeWidth = 1.8, className, color, label }: MorphIconProps) {
  return (
    <MorphIconBase
      icon={morphInput(icon)}
      size={size}
      strokeWidth={strokeWidth}
      className={className}
      color={color}
      label={label}
      reducedMotion="never"
    />
  );
}

/**
 * lucide 的数据是「一个带 children 的 svg 节点」，morphicons 要的是「节点列表」，
 * 中间差一层 children。形状适配只留这一处：两边的类型都由各自的包定义，
 * 谁都不认识对方，散落到每个调用点只会变成一堆断言。
 */
function morphInput(icon: IconNode): IconInput {
  const [, , children] = icon;
  return (children ?? []) as unknown as IconInput;
}
