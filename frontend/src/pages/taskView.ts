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

/**
 * 角色 ID 的展示文案。
 *
 * 角色是按点分层级的标识（`software.implementer`），界面只显示最后一段再看不懂，
 * 所以维护一张常见角色的中文表，其余回退到原始 ID——宁可显示 `software.auditor`
 * 也不猜一个错误的中文名。
 */
const ROLE_LABELS: Record<string, string> = {
  'software.analyst': '需求分析',
  'software.implementer': '实现',
  'software.verifier': '测试',
  'software.reviewer': '代码审查',
  'software.delivery': '交付',
};

export function roleLabel(roleId: string | null | undefined) {
  const value = (roleId || '').trim();
  if (!value) return '';
  return ROLE_LABELS[value] || value;
}

/** 角色徽标的短名：`software.implementer` → `implementer`，用于窄列。 */
export function roleShortName(roleId: string | null | undefined) {
  const value = (roleId || '').trim();
  if (!value) return '';
  const tail = value.slice(value.lastIndexOf('.') + 1);
  return tail || value;
}


/**
 * 父子协作关系的文案。
 *
 * `delegate` 是「换了个角色做」，`retry` 是「同一步重做」——两者都挂在父运行下，
 * 但用户看到的意思完全不同，不能都显示成「子任务」。
 */
export function parentKindLabel(kind: string | null | undefined) {
  const value = (kind || '').trim();
  if (value === 'delegate') return '委派';
  if (value === 'retry') return '重做';
  return '';
}

export type TeamSummary = {
  delegated: number;
  retries: number;
  running: number;
};

/**
 * 归一化团队的协作计数。
 *
 * 后端给的 `team` 是可选字段：旧数据或技能运行没有它。任何一项缺失都按 0 处理，
 * 全部为 0 时返回 null，界面据此整块不渲染——不显示一排 `0 委派` 的空指标。
 */
export function teamSummary(team: unknown): TeamSummary | null {
  if (!team || typeof team !== 'object') return null;
  const source = team as Record<string, unknown>;
  const pick = (key: string) => {
    const value = source[key];
    return typeof value === 'number' && Number.isFinite(value) && value > 0 ? Math.floor(value) : 0;
  };
  const summary: TeamSummary = {
    delegated: pick('delegated'),
    retries: pick('retries'),
    running: pick('running'),
  };
  return summary.delegated || summary.retries || summary.running ? summary : null;
}

export type RoleChip = { id: string; label: string; status: string };

/**
 * 归一化「这条运行里出现过的角色」。
 *
 * 顺序保持后端给的出现顺序：它反映步骤推进顺序，按名字排序会把协作链路打乱。
 * 没有角色、role 缺 id、或重复 id 都跳过；结果为空时返回空数组，
 * 调用方据此完全不渲染角色链。
 */
export function roleChips(roles: unknown): RoleChip[] {
  if (!Array.isArray(roles)) return [];
  const seen = new Set<string>();
  const chips: RoleChip[] = [];
  for (const item of roles) {
    if (!item || typeof item !== 'object') continue;
    const source = item as Record<string, unknown>;
    const id = typeof source.id === 'string' ? source.id.trim() : '';
    if (!id || seen.has(id)) continue;
    seen.add(id);
    chips.push({
      id,
      label: roleLabel(id),
      status: typeof source.status === 'string' ? source.status : '',
    });
  }
  return chips;
}

/**
 * 是否显示角色徽标。
 *
 * 没有角色就不显示：工作台任务和技能运行本来就没有角色，凭空补一个「未分配」
 * 会让所有记录突然多一个无意义的标签。
 */
export function shouldShowRoleBadge(roleId: string | null | undefined) {
  return Boolean((roleId || '').trim());
}

/**
 * 是否显示角色协作链路。
 *
 * 至少两个角色才算「协作」：一个角色只是「谁在做」，徽标已经说完了；
 * 画成链路只是多占一行。
 */
export function shouldShowRoleChain(roles: unknown) {
  return roleChips(roles).length > 1;
}

