/**
 * 任务中心的纯展示逻辑。
 *
 * 任务类型/状态的文案、状态归类和相对时间格式化同时被侧栏的「正在执行」条和
 * 任务中心页使用，放在一个模块里避免两处措辞漂移；抽成纯函数也便于自检脚本
 * 覆盖时间分桶这类容易写错的边界。
 */

export const TASK_ACTIVE_STATUSES = new Set(['pending', 'running', 'canceling']);
export const TASK_COMPLETED_STATUSES = new Set(['completed', 'success', 'done', 'finished']);
// `waiting` 来自本机运行台账：等待审批/反馈既没结束也没失败，但需要人来推进。
export const TASK_ATTENTION_STATUSES = new Set(['failed', 'canceled', 'waiting']);

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
  return ({ pending: '等待中', running: '运行中', canceling: '取消中', waiting: '等待确认', completed: '已完成', failed: '失败', canceled: '已取消' } as Record<string, string>)[status] || status || '未知';
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

/**
 * 本机台账的运行号形如 `workflow_run_inv_1790228440476_15:1790228443195-41`，
 * 直接铺在列表 meta 行里会把同一行的其它信息挤没。列表只留能区分彼此的尾号，
 * 完整值交给 `title` 和运行详情，口径与「我的能力 → 工作流」的运行号一致。
 */
export function shortRunId(id: string) {
  const trimmed = (id || '').trim();
  if (trimmed.length <= 12) return trimmed;
  // 本机运行号形如 `...:<毫秒时间戳>_<进程号>`：冒号前是一次交互的标识，尾部进程号只有
  // 排查时才有意义。用户要的是「这条运行」的可辨识尾号，所以两段都剥掉再取尾号。
  const tail = trimmed.slice(trimmed.lastIndexOf(':') + 1).replace(/_\d+$/, '');
  const compact = tail.replace(/[^0-9a-zA-Z]/g, '');
  return `#${(compact || tail).slice(-6)}`;
}

/// 进度取整并夹到 0-100；拿不到数字（本机技能运行没有步骤指标）时返回 null。
export function taskProgressValue(progress: number | null | undefined) {
  if (typeof progress !== 'number' || !Number.isFinite(progress)) return null;
  return Math.max(0, Math.min(100, Math.round(progress)));
}

/// 进度未知说「进行中」，不能把「不知道」显示成 0%——那会被读成「一步都没动」。
export function taskProgressLabel(progress: number | null | undefined) {
  const value = taskProgressValue(progress);
  return value === null ? '进行中' : `${value}%`;
}

/// 步骤文案只在真的有步骤总数时出现，避免「步骤 0/0」这种空指标。
export function taskStepLabel(done: number | null | undefined, total: number | null | undefined) {
  if (typeof done !== 'number' || typeof total !== 'number') return '';
  if (!Number.isFinite(done) || !Number.isFinite(total) || total <= 0) return '';
  return `步骤 ${Math.max(0, Math.min(Math.round(total), Math.round(done)))}/${Math.round(total)}`;
}

/// 进行中的行要显示走动的「已运行」；起点缺失或不可解析时返回 null，宁可不显示。
export function taskElapsedSeconds(startedAt: string | null | undefined, now: number) {
  if (!startedAt) return null;
  const started = Date.parse(startedAt);
  if (Number.isNaN(started)) return null;
  return Math.max(0, Math.round((now - started) / 1000));
}
