/**
 * AI 连接页签与页头说明的唯一事实源。
 *
 * 页签 id 保持稳定（外部脚本与深链依赖 mcp / services / acp），标签则要给三处共用：
 * 页内页签、页头说明、以及对话页报错时的指路文案。指路文案里的词一旦和页签分叉，
 * 用户就会拿着报错原话去找一个界面上并不存在的入口。
 */
export type AiConnectionsTab = 'mcp' | 'services' | 'routes' | 'acp';

export const AI_CONNECTIONS_TAB_ORDER: readonly AiConnectionsTab[] = ['mcp', 'services', 'routes', 'acp'];

export const AI_CONNECTIONS_TAB_LABELS: Record<AiConnectionsTab, string> = {
  mcp: '工具接入',
  services: 'AI 服务',
  routes: '能力路由',
  acp: '运行环境',
};

/** 左栏条目说明与页头说明共用同一句话。 */
export const AI_CONNECTIONS_DESCRIPTION = '接入本机 AI 工具，管理 AI 服务、能力路由与运行环境。';
