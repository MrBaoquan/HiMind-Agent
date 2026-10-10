// 「本机工具」的信息架构：把三份注册表（MCP 目标 / 模型分发客户端 / ACP 执行器）
// 按工具身份合并成一行，每个工具在同屏里管好自己的全部接线。
//
// 为什么不把这个合并做进后端：三份清单分别由 MCP 探测、供应商簿记、ACP 登记产生，
// 端上就近合并能立刻验证结构，且不会把「工具身份」这个还在演进的映射固化成协议。
// 映射不确定的地方一律按事实显示（例如只有 MCP、没有模型分发能力的工具标「仅接入」），
// 不猜、不静默回落。

import type {
  AcpRuntimeProfile,
  AcpRuntimeProfileSnapshot,
  AIProviderImportStatus,
  CustomAIService,
  ManagedAIServiceSummary,
  McpTargetDescriptor,
} from '../services/agentApi';
import { acpPresets } from './acpProfileView';
import { clientLabel } from '../utils/clientLabels';

export type ToolCapabilities = { mcp: boolean; model: boolean; exec: boolean };

/**
 * 一个工具在一行里只能有一种状态，三选一：
 *   attention = 有东西坏了要修（配置异常 / 需更新）；
 *   ready     = 该接的都接好了，或本来就无需再动（内置）；
 *   idle      = 可选接线还没接（多是用户未必想接的工具）。
 * 把「能接但没接」和「接坏了」分开，是为了让「待处理」只留真需要处理的，
 * 否则一屏十几个琥珀色标签，真正的异常反而被淹没。
 */
export type ToolState = 'attention' | 'idle' | 'ready';

export type ToolConnection = {
  /** 稳定工具身份：MCP 目标 id / 模型客户端 target / ACP 的 `acp.<id>` 去掉前缀后同一个。 */
  key: string;
  name: string;
  mcp: McpTargetDescriptor | null;
  model: AIProviderImportStatus | null;
  exec: AcpRuntimeProfile | null;
  capabilities: ToolCapabilities;
  detected: boolean;
  builtin: boolean;
  state: ToolState;
};

export const BUILTIN_TOOL_KEY = 'himind-ai';

/** ACP 预设里的 providerId 就是工具身份（codex / opencode / github-copilot …）。 */
const execPresetKeys = new Set(acpPresets.map((preset) => preset.providerId));

/** 共享展示名优先；MCP 探测给了更具体的名字时用它，最后才回落到 id。 */
function displayName(key: string, mcp: McpTargetDescriptor | null, exec: AcpRuntimeProfile | null): string {
  const shared = clientLabel(key);
  if (shared !== key) return shared;
  if (mcp?.name) return mcp.name;
  if (exec?.display_name) return exec.display_name;
  return key;
}

export function toolState(item: ToolConnection): ToolState {
  if (item.builtin) return 'ready';
  const mcp = item.mcp;
  if (mcp && (mcp.state === 'needs_repair' || mcp.state === 'invalid_config')) return 'attention';
  const mcpSettled = !mcp || mcp.state === 'configured';
  const modelSettled = !item.model || item.model.state === 'imported';
  const execSettled = !item.exec || item.exec.enabled;
  return mcpSettled && modelSettled && execSettled ? 'ready' : 'idle';
}

// 排序：内置置顶 → 待处理 → 已就绪 → 未接入。把已接好的排在未接入前面，
// 接一个工具它就往上走一格，列表底部留给那串可选的未接入工具。
const STATE_RANK: Record<ToolState, number> = { attention: 1, ready: 2, idle: 3 };

function rank(item: ToolConnection): number {
  return item.builtin ? 0 : STATE_RANK[item.state];
}

export function buildToolConnections(input: {
  targets: McpTargetDescriptor[];
  clients: AIProviderImportStatus[];
  acp: AcpRuntimeProfileSnapshot | null;
}): ToolConnection[] {
  const map = new Map<string, ToolConnection>();
  const ensure = (key: string): ToolConnection => {
    let item = map.get(key);
    if (!item) {
      item = {
        key,
        name: key,
        mcp: null,
        model: null,
        exec: null,
        capabilities: { mcp: false, model: false, exec: false },
        detected: false,
        builtin: key === BUILTIN_TOOL_KEY,
        state: 'idle',
      };
      map.set(key, item);
    }
    return item;
  };

  for (const target of input.targets) ensure(target.id).mcp = target;
  for (const client of input.clients) ensure(client.target).model = client;
  for (const profile of input.acp?.profiles ?? []) ensure(profile.provider_id.replace(/^acp\./i, '')).exec = profile;

  const items = [...map.values()].map((item) => {
    item.name = displayName(item.key, item.mcp, item.exec);
    item.capabilities = {
      mcp: Boolean(item.mcp),
      model: Boolean(item.model),
      exec: Boolean(item.exec) || execPresetKeys.has(item.key),
    };
    item.detected = Boolean(item.mcp?.detected || item.model?.client_detected || item.exec);
    item.state = toolState(item);
    return item;
  });

  return items.sort((a, b) => {
    const byRank = rank(a) - rank(b);
    if (byRank) return byRank;
    return a.name.localeCompare(b.name, 'zh-Hans-CN');
  });
}

/** 模型来源的展示名：把簿记里的 `managed` / `custom:<id>` / 空值翻成人话。 */
export function modelSourceLabel(service: string | undefined | null, services: CustomAIService[], managed: ManagedAIServiceSummary): string {
  if (!service) return '来源不明';
  if (service === 'managed') return managed.available ? '工作台模型服务' : '工作台模型服务（未就绪）';
  if (service.startsWith('custom:')) {
    const id = service.slice('custom:'.length);
    return services.find((candidate) => candidate.id === id)?.display_name ?? '已删除的服务';
  }
  return '其他服务';
}
