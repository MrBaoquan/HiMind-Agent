/**
 * 任务中心的纯展示逻辑。
 *
 * 任务类型/状态的文案、状态归类和相对时间格式化同时被侧栏的「正在执行」条和
 * 任务中心页使用，放在一个模块里避免两处措辞漂移；抽成纯函数也便于自检脚本
 * 覆盖时间分桶这类容易写错的边界。
 */

export const TASK_ACTIVE_STATUSES = new Set(['pending', 'running', 'canceling']);
export const TASK_COMPLETED_STATUSES = new Set(['completed', 'success', 'done', 'finished']);
export const TASK_ATTENTION_STATUSES = new Set(['failed', 'canceled']);

export type TaskTone = 'running' | 'pending' | 'success' | 'danger' | 'neutral';

export function isTaskActive(status: string) {
  return TASK_ACTIVE_STATUSES.has(status);
}

export function taskTypeLabel(taskType: string) {
  const labels: Record<string, string> = {
    upload_code: '代码上传',
    upload_placeholder: '准备文件上传',
    smb_upload: '共享目录上传',
    sync_exhibits: '项目同步',
    initialize_exhibit_repository: '项目初始化',
    agent_run: 'AI 任务',
  };
  return labels[taskType] || taskType || '远程任务';
}

export function taskStatusLabel(status: string) {
  return ({ pending: '等待中', running: '运行中', canceling: '取消中', completed: '已完成', failed: '失败', canceled: '已取消' } as Record<string, string>)[status] || status || '未知';
}

export function taskStatusTone(status: string): TaskTone {
  if (TASK_COMPLETED_STATUSES.has(status)) return 'success';
  if (TASK_ATTENTION_STATUSES.has(status)) return status === 'failed' ? 'danger' : 'neutral';
  if (status === 'pending') return 'pending';
  return 'running';
}

export function formatAbsoluteTime(value?: string | null) {
  if (!value) return '--';
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString('zh-CN', { hour12: false });
}

/// 相对时间只在列表里用，超过一周回退到绝对时间；非法输入原样返回，不显示“刚刚”。
export function formatRelativeTime(value?: string | null, now: number = Date.now()) {
  if (!value) return '--';
  const timestamp = new Date(value).getTime();
  if (Number.isNaN(timestamp)) return value;
  const seconds = Math.max(0, Math.floor((now - timestamp) / 1000));
  if (seconds < 60) return '刚刚';
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分钟前`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)} 小时前`;
  if (seconds < 604800) return `${Math.floor(seconds / 86400)} 天前`;
  return formatAbsoluteTime(value);
}

/// 缺少起止时间或时间无法解析时不显示耗时，避免出现 "NaN 秒"。
export function formatDuration(start?: string | null, end?: string | null) {
  if (!start || !end) return '';
  const started = new Date(start).getTime();
  const finished = new Date(end).getTime();
  if (Number.isNaN(started) || Number.isNaN(finished)) return '';
  const seconds = Math.max(0, Math.round((finished - started) / 1000));
  if (seconds === 0) return '< 1 秒';
  if (seconds < 60) return `${seconds} 秒`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分 ${seconds % 60} 秒`;
  return `${Math.floor(seconds / 3600)} 小时 ${Math.floor((seconds % 3600) / 60)} 分`;
}

/// 任务归属分类：进行中 / 已完成 / 需处理之外的未知状态单独返回，避免被算进已完成。
export function taskBucket(status: string): 'active' | 'completed' | 'attention' | 'unknown' {
  if (TASK_ACTIVE_STATUSES.has(status)) return 'active';
  if (TASK_COMPLETED_STATUSES.has(status)) return 'completed';
  if (TASK_ATTENTION_STATUSES.has(status)) return 'attention';
  return 'unknown';
}
