/**
 * 能力的类型口径：插件 / 技能 / 工作流，各自的文案与顺序。
 *
 * 市场、我的能力、组织管理三处都要给条目标类型，文案只能有一份，
 * 否则同一类东西在两处会写出不同的叫法。图标是 UI 的事，跟着这份类型放在
 * components/ExtensionKindMark 里。
 */
export type ExtensionKind = 'plugin' | 'skill' | 'workflow';

/// MCP 工具是「接进来的本机工具」，没有签名、没有版本更新，所以不进
/// extensionKindOrder（那份口径被安装、签名、分发、扩展开发共用）。但它和
/// 插件 / 技能 / 工作流一样是「我拥有的能力」，所以在市场和「我的能力」里占一个页签。
export type McpKind = 'mcp';

/// 「组织管理」不是一类能力，而是能力的一种来源口径，所以不在 extensionKindOrder 里。
export type CapabilityKind = ExtensionKind | McpKind | 'policy';

export const extensionKindOrder: ExtensionKind[] = ['plugin', 'skill', 'workflow'];
export const capabilityKindOrder: CapabilityKind[] = ['plugin', 'skill', 'workflow', 'mcp', 'policy'];
/// 市场页签：四类可获得的制品。市场里没有「组织管理」，它只在「我的能力」下。
export const marketKindOrder: (ExtensionKind | McpKind)[] = ['plugin', 'skill', 'workflow', 'mcp'];

export const extensionKindLabels: Record<ExtensionKind, string> = { plugin: '插件', skill: '技能', workflow: '工作流' };
export const capabilityKindLabels: Record<CapabilityKind, string> = { ...extensionKindLabels, mcp: 'MCP 工具', policy: '组织管理' };
