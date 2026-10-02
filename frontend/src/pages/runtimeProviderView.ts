// 「运行环境」的展示口径只保留一份。
//
// 运行环境 = 真正执行 AI 步骤的后端，由两类来源拼成：
//   1. 本机探测到的执行后端（himind.builtin / personal.codex / personal.github-copilot）
//   2. 用户接入的 ACP 客户端（acp.*）
// 工作流详情、AI 连接页、设置页都从这里取名。原先每个页面各写一遍品牌子串判断，
// 结果是同一个 provider 在三处可能显示三个名字。
//
// 本文件刻意不依赖 react / tauri，check-runtime-providers.mts 可以直接跑断言。

import { acpPresets } from './acpProfileView.ts';
import type { AcpRuntimeProfile } from '../services/agentApi';

export type LocalRuntimeIconName = 'himind-ai' | 'code' | 'github' | 'target';

export type LocalRuntimeMeta = {
  name: string;
  detail: string;
  icon: LocalRuntimeIconName;
};

/**
 * 本机自带执行后端固定是这三家，后端 `runtime::probe_installations` 也只探测这三家。
 * 探测失败只影响状态，不影响这里能不能显示名字。
 */
// detail 只描述「这是什么」，不描述「装没装」：写成「本机安装的 Copilot CLI」时，
// 未安装的后端会显示成「本机安装的 Copilot CLI · 未安装」这种自相矛盾的一行。
export const LOCAL_RUNTIME_META: Record<string, LocalRuntimeMeta> = {
  'himind.builtin': { name: 'HiMind AI', detail: '内置执行引擎', icon: 'himind-ai' },
  'personal.codex': { name: 'Codex', detail: 'CLI 执行后端', icon: 'code' },
  'personal.github-copilot': { name: 'GitHub Copilot', detail: 'CLI 执行后端', icon: 'github' },
};

/** 未知后端不隐藏：露出 provider id，用户至少知道该去哪儿查。 */
export function localRuntimeMeta(provider: string): LocalRuntimeMeta {
  return LOCAL_RUNTIME_META[provider] ?? { name: provider, detail: '本机执行后端', icon: 'target' };
}

/**
 * 探测状态 → 中文短语 + 配色。
 * `unsupported` 是「装了但当前 Agent 用不了」，和「没装」不是一回事，不能都写成灰色。
 */
export function localRuntimeStatus(status: string): { label: string; kind: 'success' | 'warn' | 'danger' | 'neutral' } {
  // 「已就绪」和下面「已接入的运行环境」用同一个词，也和概览里的「已就绪」对齐。
  if (status === 'ready') return { label: '已就绪', kind: 'success' };
  if (status === 'incompatible') return { label: '需要更新', kind: 'warn' };
  if (status === 'unsupported') return { label: '不可用', kind: 'danger' };
  return { label: '未安装', kind: 'neutral' };
}

/** 和后端 `runtime::acp::is_provider` 同口径，别在别处另写前缀判断。 */
export function isAcpProvider(provider: string): boolean {
  return provider === 'acp.stdio' || provider.startsWith('acp.');
}

/**
 * 运行环境展示名。
 *
 * `acp.*` 必须先判：`acp.github-copilot` 里也含 "copilot"，先走品牌子串分支会把
 * 用户自己接入的客户端认成本机的 GitHub Copilot CLI。
 *
 * 客户端被删掉后仍按内置预设名兜底，历史运行记录不会只剩一串 `acp.xxx`。
 */
export function runtimeProviderLabel(provider?: string, profiles: AcpRuntimeProfile[] = []): string {
  if (!provider) return '自动选择';
  if (isAcpProvider(provider)) {
    const profile = profiles.find(candidate => candidate.provider_id === provider);
    if (profile?.display_name) return profile.display_name;
    const bare = provider.replace(/^acp\./i, '').toLowerCase();
    return acpPresets.find(preset => preset.providerId === bare)?.name || provider;
  }
  const meta = LOCAL_RUNTIME_META[provider];
  if (meta) return meta.name;
  // 兜底：历史记录里出现过带品牌的 provider id，按品牌认出来比显示一串 ID 强。
  if (provider.includes('deepseek')) return 'HiMind AI';
  if (provider.includes('codex')) return 'Codex';
  if (provider.includes('copilot')) return 'GitHub Copilot';
  return '自定义运行环境';
}
