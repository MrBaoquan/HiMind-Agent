import { AlertCircle } from 'lucide-react';
import './WorkspaceFeedback.css';

export type WorkspaceIssue = { message: string; detail?: string };

export function workspaceIssue(reason: unknown, fallback: string): WorkspaceIssue {
  const detail = String(reason).replace(/^Error:\s*/, '');
  const hints: Array<[string, string]> = [
    ['WORKSPACE_REGISTRY_CHANGED', '项目登记已变化，请刷新后重新预览或搜索。'],
    ['PATH_CONFLICT', '目录已关联其他项目，请先解除原关联。'],
    ['PROJECT_NOT_FOUND', '项目已不存在，请刷新工作区。'],
    ['PROJECT_HAS_CHILDREN', '请先调整子项目的上级，再删除此项目。'],
    ['PROJECT_HAS_WORKSPACES', '请先解除目录关联，再删除此项目。'],
    ['PARENT_CYCLE', '项目不能互为上级，请更换上级项目。'],
    ['ALIAS_CONFLICT', '别名已被占用，请更换别名。'],
    ['PATH_NOT_ABSOLUTE', '请使用盘符或 UNC 开头的绝对路径。'],
    ['PATH_INVALID', '路径格式有误，请检查路径后重试。'],
    ['SESSION_CURSOR_INVALID', '会话列表已变化，请刷新后重试。'],
  ];
  return { message: hints.find(([code]) => detail.includes(code))?.[1] ?? fallback, detail };
}

export function WorkspaceFeedback({ issue, onRetry, retryLabel = '重试', disabled = false }: {
  issue: WorkspaceIssue; onRetry?: () => void; retryLabel?: string; disabled?: boolean;
}) {
  return <div className="workspace-feedback">
    <p role="alert"><AlertCircle size={16} aria-hidden="true" />{issue.message}</p>
    {onRetry ? <button type="button" className="btn" disabled={disabled} onClick={onRetry}>{retryLabel}</button> : null}
    {issue.detail ? <details><summary>诊断详情</summary><pre tabIndex={0}>{issue.detail}</pre></details> : null}
  </div>;
}
