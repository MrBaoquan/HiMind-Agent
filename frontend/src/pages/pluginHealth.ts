import type { PluginItem } from '../services/agentApi';

/// 失败记录会保留到下一次成功调用，所以"要不要把这条记录摆到用户面前"必须先分清新旧。
export const PLUGIN_FAILURE_RECENCY_MS = 24 * 60 * 60 * 1000;

export function pluginFailureState(item: PluginItem) {
  const at = typeof item.last_failure_at === 'number' ? item.last_failure_at * 1000 : null;
  const stale = at !== null && !item.circuit_open && Date.now() - at > PLUGIN_FAILURE_RECENCY_MS;
  return { at, stale };
}

/// 熔断期内、或 24 小时内的失败记录，都会让依赖它的技能降级甚至不可用，用户需要能自己解封，
/// 所以这两类情况给「修复并重试」；超过 24 小时的陈旧记录会在下一次成功调用时自动清掉，
/// 再摆一个修复入口只会制造噪音。
export function pluginNeedsRepair(item: PluginItem) {
  const { stale } = pluginFailureState(item);
  return Boolean(item.circuit_open || item.status === 'failed' || (item.error && !stale));
}
