export type PageKey = 'dashboard' | 'builtin-ai' | 'ai' | 'approvals' | 'inbox' | 'tasks' | 'workflows' | 'schedules' | 'extensions' | 'installed' | 'development' | 'settings' | 'logs';

/**
 * 「我的能力」页内的类型页签：三类可安装能力 + 组织策略。
 * 类型是页内页签，不再是侧栏项——市场负责获得，我的能力负责拥有。
 */
export type InstalledKind = 'plugin' | 'skill' | 'workflow' | 'expert' | 'instruction' | 'mcp' | 'policy';

/**
 * 页面级导航仍可直接传 PageKey；需要恢复任务上下文时传结构化目标。
 * 当前由 Agent Shell 消费，后续可无缝映射到 URL/deep link。
 */
export type NavigationTarget =
  | PageKey
  | { page: PageKey; runId?: string; workflowId?: string; kind?: InstalledKind };

export type UiMessage = {
    id: number;
    kind: 'success' | 'error' | 'info';
    text: string;
};

export function errorDetail(error: unknown): string {
    if (typeof error === 'string' && error.trim()) return error.trim();
    if (error instanceof Error && error.message) return error.message;
    return '';
}

export function formatError(error: unknown, fallback: string): string {
    const detail = errorDetail(error).toLowerCase();
    if (detail.includes('timed out') || detail.includes('timeout') || detail.includes('读取超时')) return `${fallback}，请稍后重试`;
    if (detail.includes('permission denied') || detail.includes('access is denied') || detail.includes('拒绝访问')) return `${fallback}，请检查权限后重试`;
    if (detail.includes('connection refused') || detail.includes('network') || detail.includes('dns') || detail.includes('网络')) return `${fallback}，请检查网络后重试`;
    // 不要吞掉真实原因：只给一句通用文案，用户和排障都拿不到信息。
    const raw = errorDetail(error);
    return raw ? `${fallback}：${raw}` : fallback;
}
