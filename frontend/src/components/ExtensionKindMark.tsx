import { BookOpen, Cable, Puzzle, ShieldCheck, Workflow, type LucideIcon } from 'lucide-react';
import type { CapabilityKind, ExtensionKind } from '../data/extensionKinds';

/**
 * 每一类能力的默认图标。
 *
 * 市场里的条目只有名字和说明，没有自带图标；没有图标时，列表行、详情头部都得有
 * 东西指认「这是什么」。所以每类固定一个形状：插件是拼块（装上去多一块能力）、
 * 技能是可查的手册、工作流是流程连线；颜色沿用类型原有的蓝 / 绿 / 琥珀三色。
 * 图标取自各页已经在用的那几个，换到哪一页都是同一个形状。
 */
export const extensionKindIcons: Record<ExtensionKind, LucideIcon> = {
  plugin: Puzzle,
  skill: BookOpen,
  workflow: Workflow,
};

/// 「组织管理」是治理口径，不是一类能力，只在页签里出现，用的是这一带已经在用的盾牌。
/// MCP 工具用接口线，和三个自研类型的形状区分开。
export const capabilityKindIcons: Record<CapabilityKind, LucideIcon> = {
  ...extensionKindIcons,
  mcp: Cable,
  policy: ShieldCheck,
};

type ExtensionKindMarkProps = {
  kind: ExtensionKind;
  size?: number;
  /// 图标单独出现、旁边没有类型文字时（例如市场列表行）传文案，读屏和悬停都认得出。
  label?: string;
};

export function ExtensionKindMark({ kind, size = 19, label }: ExtensionKindMarkProps) {
  const Icon = extensionKindIcons[kind];
  const glyph = <Icon size={size} strokeWidth={1.8} />;
  return label
    ? <span className={`extension-kind-mark ${kind}`} role="img" aria-label={label} title={label}>{glyph}</span>
    : <span className={`extension-kind-mark ${kind}`} aria-hidden="true">{glyph}</span>;
}
