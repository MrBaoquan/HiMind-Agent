import type {
  AIServiceProtocol,
  CustomAIService,
  GatewayModelRoute,
  GatewayProfile,
} from '../services/agentApi';

/** 客户端入口协议的固定顺序同时用于界面展示和后端测试断言。 */
export const GATEWAY_ENTRY_PROTOCOLS: AIServiceProtocol[] = [
  'openai-chat',
  'openai-responses',
  'anthropic',
];

export const GATEWAY_PROTOCOL_LABELS: Record<AIServiceProtocol, string> = {
  'openai-chat': 'OpenAI Chat',
  'openai-responses': 'OpenAI Responses',
  anthropic: 'Anthropic Messages',
};

export const GATEWAY_CLIENT_HINTS: Record<AIServiceProtocol, string> = {
  'openai-chat': 'OpenCode / 通用客户端',
  'openai-responses': 'Codex',
  anthropic: 'Claude Code / Desktop',
};

export const GATEWAY_PROTOCOL_SHORT_LABELS: Record<AIServiceProtocol, string> = {
  'openai-chat': 'Chat',
  'openai-responses': 'Responses',
  anthropic: 'Anthropic',
};

const UPSTREAM_PROTOCOLS: AIServiceProtocol[] = [
  'openai-responses',
  'anthropic',
  'openai-chat',
];

/** 与 Agent 后端 validate_routes 的协议矩阵保持一致。 */
export function upstreamSupportsEntry(
  upstream: AIServiceProtocol,
  entry: AIServiceProtocol,
): boolean {
  if (upstream === 'openai-chat') return true;
  if (upstream === 'openai-responses') {
    return entry === 'openai-responses' || entry === 'anthropic';
  }
  return upstream === 'anthropic' && entry === 'anthropic';
}

export function gatewayProfileProtocols(
  profile: Pick<GatewayProfile, 'entry_protocol' | 'entry_protocols'>,
): AIServiceProtocol[] {
  const configured = Array.isArray(profile.entry_protocols) ? profile.entry_protocols : [];
  const values = configured.length ? configured : [profile.entry_protocol];
  return GATEWAY_ENTRY_PROTOCOLS.filter(protocol => values.includes(protocol));
}

export function setGatewayEntryProtocol(
  current: AIServiceProtocol[],
  protocol: AIServiceProtocol,
  selected: boolean,
): AIServiceProtocol[] {
  const next = selected
    ? [...current, protocol]
    : current.filter(candidate => candidate !== protocol);
  if (!next.length) return current;
  return GATEWAY_ENTRY_PROTOCOLS.filter(candidate => next.includes(candidate));
}

/** 单个客户端入口可调用的上游协议。 */
export function supportedUpstreamProtocolsForEntry(
  entry: AIServiceProtocol,
): AIServiceProtocol[] {
  return UPSTREAM_PROTOCOLS.filter(upstream => upstreamSupportsEntry(upstream, entry));
}

/** 当前服务协议会被注册到哪些已选客户端。 */
export function compatibleEntryProtocolsForUpstream(
  upstream: AIServiceProtocol,
  entries: AIServiceProtocol[],
): AIServiceProtocol[] {
  return entries.filter(entry => upstreamSupportsEntry(upstream, entry));
}

/** 仅用于兼容性概览：所有已选客户端都能使用的交集。 */
export function compatibleUpstreamProtocols(
  entries: AIServiceProtocol[],
): AIServiceProtocol[] {
  if (!entries.length) return [];
  return UPSTREAM_PROTOCOLS.filter(upstream =>
    entries.every(entry => upstreamSupportsEntry(upstream, entry)));
}

/** 一条模型路由只要兼容任一已选客户端，就应该进入该 Profile。 */
export function serviceSupportsGatewayProtocols(
  service: Pick<CustomAIService, 'protocol'>,
  entries: AIServiceProtocol[],
): boolean {
  return compatibleEntryProtocolsForUpstream(service.protocol, entries).length > 0;
}

export function supportedUpstreamLabelForEntry(entry: AIServiceProtocol): string {
  return supportedUpstreamProtocolsForEntry(entry)
    .map(protocol => GATEWAY_PROTOCOL_LABELS[protocol])
    .join(' / ');
}

export function compatibleUpstreamLabel(entries: AIServiceProtocol[]): string {
  return compatibleUpstreamProtocols(entries)
    .map(protocol => GATEWAY_PROTOCOL_LABELS[protocol])
    .join(' / ') || '无';
}

export function gatewayCompatibilityMessage(entries: AIServiceProtocol[]): string {
  if (!entries.length) return '至少选择一个协议。';
  return '每个协议至少保留一个启用模型';
}

export function gatewayProtocolSummary(entries: AIServiceProtocol[]): string {
  return entries.map(protocol => GATEWAY_PROTOCOL_SHORT_LABELS[protocol]).join(' + ');
}

export function visibleProfileModels(
  routes: GatewayModelRoute[],
  expanded: boolean,
  limit = 3,
): GatewayModelRoute[] {
  return expanded ? routes : routes.slice(0, limit);
}
