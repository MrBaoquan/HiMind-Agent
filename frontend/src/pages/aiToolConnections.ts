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
  /** 需要用户处理：能接但没接、或接得不对。 */
  attention: boolean;
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

function mcpWantsAttention(mcp: McpTargetDescriptor | null): boolean {
  if (!mcp) return false;
  if (mcp.state === 'needs_repair' || mcp.state === 'invalid_config') return true;
  return mcp.detected && mcp.state !== 'configured';
}

function modelWantsAttention(model: AIProviderImportStatus | null): boolean {
  if (!model) return false;
  return model.client_detected && model.state !== 'imported';
}

function computeAttention(item: ToolConnection): boolean {
  if (item.builtin) return false;
  return mcpWantsAttention(item.mcp) || modelWantsAttention(item.model);
}

function rank(item: ToolConnection): number {
  if (item.builtin) return 0;
  if (item.attention) return 1;
  if (item.detected) return 2;
  return 3;
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
        attention: false,
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
    item.attention = computeAttention(item);
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
