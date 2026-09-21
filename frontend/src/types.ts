export type PageKey = 'dashboard' | 'builtin-ai' | 'ai' | 'approvals' | 'inbox' | 'workflows' | 'schedules' | 'extensions' | 'plugins' | 'skills' | 'development' | 'settings' | 'logs';

/**
 * 页面级导航仍可直接传 PageKey；需要恢复任务上下文时传结构化目标。
 * 当前由 Agent Shell 消费，后续可无缝映射到 URL/deep link。
 */
export type NavigationTarget =
  | PageKey
  | { page: PageKey; runId?: string; workflowId?: string };

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
    return fallback;
}
