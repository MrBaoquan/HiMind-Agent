export type PageKey = 'dashboard' | 'approvals' | 'plugins' | 'settings' | 'logs';

export type UiMessage = {
    id: number;
    kind: 'success' | 'error' | 'info';
    text: string;
};

export function formatError(error: unknown, fallback: string): string {
    if (typeof error === 'string' && error.trim()) return error.trim();
    if (error instanceof Error && error.message) return error.message;
    return fallback;
}
