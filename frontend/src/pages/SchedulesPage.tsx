import { useEffect, useMemo, useRef, useState } from 'react';
import { BookOpen, CalendarClock, CircleAlert, FolderOpen, Pause, Pencil, Play, Power, Search, Store, Trash2, X } from 'lucide-react';
import { BusyIndicator } from '../components/BusyIndicator';
import { EmptyState, PageHeader, Pill } from '../components/Common';
import type { Schedule, ScheduleInput, ScheduleList, SkillRun, WorkflowCenterSnapshot, WorkflowPreflight, WorkflowRunPreset } from '../services/agentApi';
import { formatCompactStamp, formatCountdown, formatRelativeStamp, formatStamp, formatTimezone, useNowTick } from '../timeFormat';
import { formatElapsedCn, toEpochSeconds } from './workflowRunView';
import { shortRunId } from './taskView';
import {
  filledFieldCount,
  fieldsToInput,
  initialFormValues,
  invalidOptionValue,
  jsonToFormValues,
  normalizeStartField,
  type WorkflowStartField,
} from './workflowStartForm';
import { blockerDiagnostics, blockerHeadline, blockerLead, preflightNote } from './preflightNote';
import { tailPath } from './pathDisplay';

type SchedulesPageProps = {
  snapshot: WorkflowCenterSnapshot | null;
  skills: Array<{ id: string; name: string; version: string }>;
  onRefreshWorkflows: () => void;
  onLoadSchedules: () => Promise<ScheduleList>;
  /** 返回保存后的计划句柄：名称留空时由后端生成，页面靠它继续选中同一条计划。 */
  onSaveSchedule: (input: ScheduleInput) => Promise<string>;
  /**
   * `silent` 用于改名后清理旧句柄这类内部动作：用户只做了一次「保存」，
   * 不该再弹一条「定时计划已删除」，否则看起来像刚存完就被删了。
   */
  onDeleteSchedule: (id: string, options?: { silent?: boolean }) => Promise<void>;
  onRunTarget: (kind: string, targetId: string, input: Record<string, unknown>) => Promise<void>;
  onRunSkill: (skillId: string, input: Record<string, unknown>) => Promise<void>;
  onLoadWorkflowPresets: (workflowId: string) => Promise<{ presets: WorkflowRunPreset[] }>;
  onPreflightWorkflow: (workflowId: string, input: Record<string, unknown>) => Promise<WorkflowPreflight>;
  onLoadSkillRuns: (limit?: number) => Promise<{ runs: SkillRun[] }>;
  onRevealSkillRun: (runId: string) => Promise<void>;
  onPickDirectory: () => Promise<string | null>;
  /** 从工作流页跳过来时预选的 Workflow 目标。 */
  presetTargetId: string;
  onOpenExtensions: () => void;
};

type Draft = {
  id: string;
  kind: string;
  targetId: string;
  cron: string;
  entrypoint: string;
  exitpoint: string;
  enabled: boolean;
  input: string;
  inputMode: 'form' | 'json';
  form: Record<string, unknown>;
  presetId: string;
};

const EMPTY_DRAFT: Draft = {
  id: '',
  kind: 'workflow',
  targetId: '',
  cron: '0 9 * * *',
  entrypoint: '',
  exitpoint: '',
  enabled: true,
  input: '{}',
  inputMode: 'json',
  form: {},
  presetId: '',
};

const WEEKDAY_NAMES = ['周日', '周一', '周二', '周三', '周四', '周五', '周六'];

/**
 * 预设表之外的表达式再做一次保守推断：只认「固定分 + 固定时」这种没有歧义的写法，
 * 于是 `0 3 * * *` 也能说成「每天 03:00」，而不是把 `0 3 * * *` 原样端给用户。
 * 只要出现步长、范围、列表，就返回空串——猜错一句中文说明，比让用户多看一次原始表达式更糟。
 */
function describeSimpleCron(normalized: string) {
  const [minute, hour, day, month, weekday] = normalized.split(' ');
  if (month !== '*') return '';
  if (!/^\d{1,2}$/.test(minute) || !/^\d{1,2}$/.test(hour)) return '';
  if (Number(minute) > 59 || Number(hour) > 23) return '';
  const time = `${hour.padStart(2, '0')}:${minute.padStart(2, '0')}`;
  if (day === '*' && weekday === '*') return `每天 ${time}`;
  if (day === '*' && /^[0-7]$/.test(weekday)) return `${WEEKDAY_NAMES[Number(weekday) % 7]} ${time}`;
  if (weekday === '*' && /^\d{1,2}$/.test(day)) return `每月 ${Number(day)} 日 ${time}`;
  return '';
}

/** 常见 cron 的中文说明；命中不了就只显示表达式，不猜语义。 */
export function cronDescription(cron: string) {
  const normalized = cron.trim().replace(/\s+/g, ' ');
  const presets: Record<string, string> = {
    '* * * * *': '每分钟',
    '*/5 * * * *': '每 5 分钟',
    '*/15 * * * *': '每 15 分钟',
    '*/30 * * * *': '每 30 分钟',
    '0 * * * *': '每小时整点',
    '0 9 * * *': '每天 09:00',
    '0 18 * * *': '每天 18:00',
    '30 9 * * 1-5': '工作日 09:30',
    '0 9 * * 1': '每周一 09:00',
    '0 9 1 * *': '每月 1 日 09:00',
  };
  return presets[normalized] || describeSimpleCron(normalized);
}

const CRON_FIELD_NAMES = ['分钟', '小时', '日期', '月份', '星期'];
const CRON_FIELD_RANGES: Array<[number, number]> = [[0, 59], [0, 23], [1, 31], [1, 12], [0, 7]];

/**
 * 前端只做语法和范围校验，真正的调度仍由 Agent 服务负责。
 * 这样用户在保存前就能知道「少了字段」还是「某一段超出范围」，而不是只收到一个通用失败提示。
 */
export function validateCronExpression(value: string) {
  const normalized = value.trim().replace(/\s+/g, ' ');
  if (!normalized) return '请填写 Cron 表达式（分 时 日 月 周）';
  const fields = normalized.split(' ');
  if (fields.length !== 5) return 'Cron 需要 5 个字段：分 时 日 月 周，例如 0 9 * * *';

  for (let index = 0; index < fields.length; index += 1) {
    const field = fields[index];
    const [minimum, maximum] = CRON_FIELD_RANGES[index];
    if (!/^[0-9*/,-]+$/.test(field)) {
      return `第 ${index + 1} 段（${CRON_FIELD_NAMES[index]}）包含不支持的字符`;
    }
    for (const part of field.split(',')) {
      if (!part) return `第 ${index + 1} 段（${CRON_FIELD_NAMES[index]}）格式不完整`;
      const [base, step] = part.split('/');
      if (part.split('/').length > 2 || (step !== undefined && (!/^\d+$/.test(step) || Number(step) < 1))) {
        return `第 ${index + 1} 段（${CRON_FIELD_NAMES[index]}）的步长必须是正整数`;
      }
      const range = base === '*' ? null : base.split('-');
      if (range && range.length > 2) return `第 ${index + 1} 段（${CRON_FIELD_NAMES[index]}）范围格式不正确`;
      if (range) {
        for (const raw of range) {
          if (!/^\d+$/.test(raw)) return `第 ${index + 1} 段（${CRON_FIELD_NAMES[index]}）只能使用数字`;
          const number = Number(raw);
          if (number < minimum || number > maximum) return `${CRON_FIELD_NAMES[index]}必须在 ${minimum}-${maximum} 之间`;
        }
        if (range.length === 2 && Number(range[0]) > Number(range[1])) {
          return `第 ${index + 1} 段（${CRON_FIELD_NAMES[index]}）的起始值不能大于结束值`;
        }
      }
    }
  }
  return '';
}

const CRON_PRESETS: Array<{ cron: string; label: string }> = [
  { cron: '0 9 * * *', label: '每天 09:00' },
  { cron: '30 9 * * 1-5', label: '工作日 09:30' },
  { cron: '0 * * * *', label: '每小时' },
  { cron: '*/15 * * * *', label: '每 15 分钟' },
  { cron: '0 9 * * 1', label: '每周一 09:00' },
];

function targetLabel(kind: string) {
  return ({ workflow: '工作流', skill: '技能' } as Record<string, string>)[kind] || kind;
}

/**
 * 计划标题：用户认的是「这条计划在跑什么」，不是那条计划句柄。
 * 之前把用户手填的 slug（daily-tech-radar）当标题，机器串占满了最显眼的一行，
 * 反而把「技术雷达」挤到副行并被省略号截断——主次正好反了。
 * 目标查不到时（工作流/技能已被卸载）才退回计划句柄，保证列表不出现空标题。
 */
function planDisplayName(item: Schedule, resolveTarget: (kind: string, targetId: string) => string) {
  const handle = item.id?.trim() || '';
  const target = resolveTarget(item.kind, item.target_id);
  // resolveTarget 找不到时会把 target_id 原样返回，那同样是机器串，不如直接用计划句柄。
  if (target && target !== item.target_id) return target;
  return handle || target || item.kind;
}

/**
 * 标题换成目标名之后，副行要留一个能区分「同一目标下的多条计划」的标识。
 * 标题本身就是句柄时（目标已卸载）不再重复一遍。
 */
function planHandleLabel(item: Schedule, resolveTarget: (kind: string, targetId: string) => string) {
  const handle = item.id?.trim() || '';
  return handle && handle !== planDisplayName(item, resolveTarget) ? handle : '';
}

function planHasOwnName(item: Schedule) {
  const name = item.id?.trim() || '';
  return Boolean(name) && !name.startsWith('schedule-');
}

/**
 * 表单里的「任务名称」只回填用户真正起过的名字。
 * 后端自动生成的 `schedule-xxx` 是计划句柄，塞进名称框会让人以为自己在改一个看不懂的名字，
 * 留空即可，保存时仍旧沿用原来那条计划的句柄，不会多出一条。
 */
function planNameInput(item: Schedule) {
  return planHasOwnName(item) ? item.id : '';
}

// 名称最终会当计划句柄落库，字符集在这里先挡一道，别等保存时才由后端报一句 id 相关的错。
const SCHEDULE_NAME_PATTERN = /^[A-Za-z0-9._-]+$/;

function scheduleNameError(name: string) {
  if (!name) return '';
  if (!SCHEDULE_NAME_PATTERN.test(name)) return '任务名称只能用字母、数字、点、横线和下划线';
  return '';
}

/**
 * 新建计划又没起名字时，句柄由前端生成。
 * 后端按目标生成的名字是固定的，同一个工作流建第二条就会撞上第一条并把它悄悄覆盖掉，
 * 所以这里先避开已有句柄再落库。
 */
function autoScheduleId(targetId: string, taken: Set<string>) {
  const base = `schedule-${targetId}`.replace(/[/\\:]/g, '-');
  if (!taken.has(base)) return base;
  for (let index = 2; index < 100; index += 1) {
    const candidate = `${base}-${index}`;
    if (!taken.has(candidate)) return candidate;
  }
  return `${base}-${Date.now()}`;
}

/**
 * 工作区本身就是一套启动参数里的一个值：预设存了哪个目录，计划就到点按哪个目录跑。
 * 这里只负责把已经存下来的值读出来展示，不在执行路径上做任何推断。
 */
function workspaceOf(input: Record<string, unknown> | null | undefined) {
  const value = input?.workspace_root ?? input?.project_root ?? input?.source_root;
  return typeof value === 'string' ? value.trim() : '';
}

function presetInputOf(preset: WorkflowRunPreset | null) {
  const input = preset?.input;
  return input && typeof input === 'object' && !Array.isArray(input) ? (input as Record<string, unknown>) : null;
}

/**
 * 预设是「这一套参数」，计划是「什么时候跑它」：
 * 计划只保存和预设不同的覆盖项，所以到点真正用的值 = 预设基线 + 计划覆盖项。
 */
function resolvedInput(input: Record<string, unknown> | null | undefined, preset: WorkflowRunPreset | null) {
  return { ...(presetInputOf(preset) || {}), ...(input || {}) };
}

/**
 * 值比较按「人看到的」来：数字 900 和表单往返后的 "900" 是同值。
 * 不做这层抹平，第一次保存就会给每个字段留一份假覆盖，把预设后来的改动挡在外面。
 */
function sameParamValue(left: unknown, right: unknown) {
  if (left === right) return true;
  if (left === null || right === null || left === undefined || right === undefined) return false;
  if (typeof left === 'object' || typeof right === 'object') return JSON.stringify(left) === JSON.stringify(right);
  return String(left) === String(right);
}

/** 要落库的覆盖项：只留下和预设不一样的值。 */
function overrideInput(input: Record<string, unknown>, preset: WorkflowRunPreset | null) {
  const base = presetInputOf(preset);
  if (!base) return input;
  const overrides: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(input)) {
    if (!sameParamValue(value, base[key])) overrides[key] = value;
  }
  return overrides;
}

/** 入口/出口同理：和工作流预设一致就不必在计划里再存一份副本。 */
function overrideEndpoint(value: string, presetValue: string) {
  const current = value.trim();
  return current === presetValue.trim() ? '' : current;
}

function runStatusLabel(status?: string) {
  // accepted = 计划已按时触发、运行已交给运行中心；运行中心那份记录才是最终结果。
  return ({ queued: '排队中', running: '运行中', waiting: '等待处理', succeeded: '已完成', failed: '失败', canceled: '已取消', accepted: '已触发' } as Record<string, string>)[status || ''] || '尚未运行';
}

/**
 * 只有运行中心给出的终态才算「结果」。
 * 计划侧记录的 `accepted` 是「已照计划触发」，把它当成结果显示出来，
 * 用户会以为这次跑完了——上一版「上次结果 = 已触发」就是这么来的。
 */
function runOutcomeLabel(status?: string) {
  return ({ succeeded: '已完成', failed: '失败', canceled: '已取消' } as Record<string, string>)[status || ''] || '';
}

/** 计划里"存在存储里的"字段指纹：只有这些变了，右侧表单才需要跟着刷新。 */
function scheduleSignature(item: Schedule) {
  return JSON.stringify([
    item.kind || 'workflow',
    item.target_id,
    item.cron,
    item.enabled,
    item.preset_id || '',
    item.execution?.entrypoint || '',
    item.execution?.exitpoint || '',
    item.input || {},
  ]);
}

export function SchedulesPage({ snapshot, skills, onRefreshWorkflows, onLoadSchedules, onSaveSchedule, onDeleteSchedule, onRunTarget, onRunSkill, onLoadSkillRuns, onRevealSkillRun, onPickDirectory, onLoadWorkflowPresets, onPreflightWorkflow, presetTargetId, onOpenExtensions }: SchedulesPageProps) {
  const workflows = snapshot?.workflows || [];
  const [list, setList] = useState<ScheduleList | null>(null);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState('');
  const [error, setError] = useState('');
  const [selectedId, setSelectedId] = useState('');
  const [draft, setDraft] = useState<Draft>(EMPTY_DRAFT);
  // 正在编辑的计划原本存在哪个句柄下：名称留空保存时沿用它，改名时用它清理旧记录。
  const [storedId, setStoredId] = useState('');
  // 默认是「看这条计划在跑什么」，表单只在点了编辑/新建之后才展开：
  // 定时计划页的主任务是管好一批计划，不是每次都重新填一遍参数。
  const [editMode, setEditMode] = useState(false);
  const [paramsOpen, setParamsOpen] = useState(false);
  const [skillRuns, setSkillRuns] = useState<SkillRun[]>([]);
  const [presets, setPresets] = useState<WorkflowRunPreset[]>([]);
  // 这批预设属于哪个工作流：只有读到了对应工作流的预设，才能判断
  // 「计划引用的预设是不是真的没了」，否则会把「还没读完」说成「不存在」。
  const [presetsTarget, setPresetsTarget] = useState('');
  const [preflight, setPreflight] = useState<WorkflowPreflight | null>(null);
  const [preflightBusy, setPreflightBusy] = useState(false);
  // 预检没通过时记下"当时这份草稿长什么样"（见 save()）：这份草稿没变才能二次点击放行，
  // 改了任何参数这个签名就对不上，"仍然保存"会自动消失，避免拿着一次旧结论跳过后续检查。
  const [blockedSignature, setBlockedSignature] = useState('');
  // 删除计划走站内确认弹窗：和工作流详情「移除工作流」、插件页「卸载」是同一套 .modal。
  const [pendingConfirm, setPendingConfirm] = useState<{ title: string; description: string; confirmText: string; run: () => void } | null>(null);
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});
  const [scheduleSearch, setScheduleSearch] = useState('');
  const [scheduleStatusFilter, setScheduleStatusFilter] = useState<'all' | 'enabled' | 'paused' | 'failed'>('all');
  // 记录"表单当前装载的是哪条计划、装载时长什么样"，用于区分未保存改动与存储侧变更。
  const loadedRef = useRef<{ id: string; signature: string; draft: string } | null>(null);
  const draftRef = useRef(draft);
  useEffect(() => {
    draftRef.current = draft;
  }, [draft]);
  // 正在编辑时不要被后台静默刷新拽回只读视图：那会把用户填到一半的改动直接收走。
  const editModeRef = useRef(editMode);
  useEffect(() => {
    editModeRef.current = editMode;
  }, [editMode]);
  // 「这份草稿」的指纹：跑过一次预检没通过之后，只有原样不动的草稿才允许跳过检查直接存。
  // 表单态和 JSON 态都要算进来——只盯 draft.input 会漏掉结构化表单里改动的字段。
  const draftSignature = JSON.stringify([
    draft.kind,
    draft.targetId,
    draft.presetId,
    draft.cron,
    draft.entrypoint,
    draft.exitpoint,
    draft.inputMode,
    draft.input,
    draft.form,
  ]);
  const saveSkippingPreflight = blockedSignature !== '' && blockedSignature === draftSignature;

  async function loadSkillRuns() {
    try {
      const result = await onLoadSkillRuns(5);
      setSkillRuns(result.runs || []);
    } catch {
      setSkillRuns([]);
    }
  }

  const selected = useMemo(() => (list?.schedules || []).find(item => item.id === selectedId) || null, [list, selectedId]);
  // 计划这次触发出来的运行还在跑时，行内要直接显示「运行中」：
  // 用 last_run_id 精确对上运行中心的那次运行，不做目标工作流级别的猜测。
  const liveRuns = useMemo(() => {
    const map = new Map<string, { label: string; startedAt: number | null }>();
    for (const item of snapshot?.runs || []) {
      if (!['queued', 'running', 'waiting'].includes(item.run.status)) continue;
      const label = item.run.status === 'waiting'
        ? (item.interaction_request?.kind === 'external_wait' ? '等待外部信号' : '等待处理')
        : '运行中';
      map.set(item.run.run_id, { label, startedAt: toEpochSeconds(item.run.created_at) });
    }
    return map;
  }, [snapshot]);
  // 有运行中的计划时页面要「追」得快一些；闲时没必要每 15 秒读一次存储。
  const hasLiveRuns = liveRuns.size > 0;
  // 「上次结果」以运行中心那份记录为准：计划侧只记到"已触发"，真正的成败在运行上。
  const runStatusById = useMemo(() => {
    const map = new Map<string, string>();
    for (const item of snapshot?.runs || []) map.set(item.run.run_id, item.run.status);
    return map;
  }, [snapshot]);
  const targetWorkflow = useMemo(() => workflows.find(item => item.package.id === draft.targetId) || null, [draft.targetId, workflows]);
  const workflowFields = useMemo(() => (targetWorkflow?.view?.sections || []).flatMap(section => (section.fields || []).map(normalizeStartField)), [targetWorkflow]);
  const entrypoints = ((targetWorkflow?.package as unknown as { entrypoints?: Array<{ id: string; label?: string }> } | null)?.entrypoints) || [];
  const exits = ((targetWorkflow?.package as unknown as { exits?: Array<{ id: string; label?: string }> } | null)?.exits) || [];
  const totalSchedules = (list?.schedules || []).length;
  const enabledCount = (list?.schedules || []).filter(item => item.enabled).length;
  const pausedCount = (list?.schedules || []).filter(item => !item.enabled).length;
  const failedCount = (list?.schedules || []).filter(item => item.last_status === 'failed').length;
  // 概览条和筛选行都是「多了才需要」的东西：两三条计划摆一整行统计格、再压一行筛选，
  // 换来的是空白和一次多余的视觉停留。计划少了就把位置还给列表本身。
  const showSummary = totalSchedules > 5;
  const showListTools = totalSchedules >= 8;
  const cronError = validateCronExpression(draft.cron);
  // 这条计划引用的预设：只读详情要显示它的名字，让「参数从哪来」一眼可见。
  const selectedPreset = useMemo(
    () => (selected?.preset_id ? presets.find(item => item.id === selected.preset_id) || null : null),
    [presets, selected],
  );
  const draftPreset = useMemo(
    () => (draft.presetId ? presets.find(item => item.id === draft.presetId) || null : null),
    [presets, draft.presetId],
  );
  const draftPresetLabel = draftPreset ? draftPreset.label || draftPreset.id : draft.presetId;
  const requiredMissing = workflowFields.some(field => field.required && !String(draft.form[field.id] ?? '').trim());
  // 没有预设可用时参数必须先填满，就直接展开，省掉一次「调整参数」的点击。
  const paramsExpanded = paramsOpen || (!draft.presetId && requiredMissing);
  // 只读详情里显示的工作区：计划只存覆盖项，真正会用到的值要把预设基线合进来。
  const selectedResolvedInput = useMemo(
    () => resolvedInput(selected?.input, selectedPreset),
    [selected, selectedPreset],
  );
  const selectedWorkspace = workspaceOf(selectedResolvedInput);
  // 这条计划自己覆盖了几项：0 项就是「完全跟着预设跑」，详情里要说清楚。
  const selectedOverrideCount = useMemo(
    () => Object.keys(overrideInput(selected?.input || {}, selectedPreset)).length,
    [selected, selectedPreset],
  );
  const selectedPresetMissing = Boolean(selected?.preset_id)
    && !selectedPreset
    && Boolean(selected)
    && presetsTarget === selected?.target_id;
  const draftWorkspace = useMemo(() => {
    const fromForm = [draft.form.workspace_root, draft.form.source_root, draft.form.project_root]
      .find(value => typeof value === 'string' && value.trim());
    if (fromForm) return String(fromForm).trim();
    try {
      return workspaceOf(draft.input.trim() ? JSON.parse(draft.input) : {});
    } catch {
      return '';
    }
  }, [draft.form, draft.input]);
  const filledParamCount = filledFieldCount(workflowFields, draft.form);
  const filteredSchedules = useMemo(() => {
    const query = scheduleSearch.trim().toLocaleLowerCase();
    return (list?.schedules || []).filter(item => {
      if (scheduleStatusFilter === 'enabled' && !item.enabled) return false;
      if (scheduleStatusFilter === 'paused' && item.enabled) return false;
      if (scheduleStatusFilter === 'failed' && item.last_status !== 'failed') return false;
      if (!query) return true;
      const haystack = [item.id, targetName(item.kind, item.target_id), item.target_id, item.cron].join(' ').toLocaleLowerCase();
      return haystack.includes(query);
    });
  }, [list, scheduleSearch, scheduleStatusFilter, workflows, skills]);

  function targetName(kind: string, targetId: string) {
    if (kind === 'workflow') return workflows.find(item => item.package.id === targetId)?.package.name || targetId;
    if (kind === 'skill') return skills.find(item => item.id === targetId)?.name || targetId;
    return targetId;
  }

  function fieldsFor(workflowId: string) {
    const workflow = workflows.find(item => item.package.id === workflowId);
    return (workflow?.view?.sections || []).flatMap(section => (section.fields || []).map(normalizeStartField));
  }

  function inputModeFor(kind: string, fields: WorkflowStartField[]) {
    return kind === 'workflow' && fields.length ? 'form' as const : 'json' as const;
  }

  function lastResultLabel(item: Schedule) {
    // 运行中心那份记录优先：它才知道这次到底成没成。
    const status = (item.last_run_id ? runStatusById.get(item.last_run_id) : '') || item.last_status;
    if (!status) return '尚未运行';
    const outcome = runOutcomeLabel(status);
    if (outcome) return outcome;
    // 只触发、还没跑到终态时，说清楚「结果要去哪看」，而不是把中间态包装成结果。
    if (status === 'accepted') return '已触发，结果见运行中心';
    return runStatusLabel(status);
  }

  // 计划触发出来的那次运行，在运行中心里的原始记录：运行态面板要显示步骤和最近心跳。
  const selectedLiveRun = useMemo(() => {
    if (!selected?.last_run_id) return null;
    const item = (snapshot?.runs || []).find(run => run.run.run_id === selected.last_run_id);
    return item && ['queued', 'running', 'waiting'].includes(item.run.status) ? item : null;
  }, [selected, snapshot]);

  // 计划是「等在那里」的能力：只要选中了计划或列表里有触发出来的运行，
  // 就用统一的秒级心跳推进「下次执行」倒计时和运行态文案，页面不再是一张静止快照。
  const nowMs = useNowTick(Boolean(selected) || liveRuns.size > 0);

  function applySchedule(item: Schedule) {
    setSelectedId(item.id);
    setStoredId(item.id);
    setError('');
    setEditMode(false);
    setParamsOpen(false);
    const fields = fieldsFor(item.target_id);
    const input = item.input || {};
    const next: Draft = {
      id: planNameInput(item),
      kind: item.kind || 'workflow',
      targetId: item.target_id,
      cron: item.cron,
      entrypoint: item.execution?.entrypoint || '',
      exitpoint: item.execution?.exitpoint || '',
      enabled: item.enabled,
      input: JSON.stringify(input, null, 2),
      inputMode: inputModeFor(item.kind || 'workflow', fields),
      form: fields.length ? jsonToFormValues(fields, input) : {},
      presetId: item.preset_id || '',
    };
    setDraft(next);
    // 记下"从存储装载时的样子"，后面靠它区分"用户改了没保存"和"存储里被别处改了"。
    loadedRef.current = { id: item.id, signature: scheduleSignature(item), draft: JSON.stringify(next) };
    setFieldErrors({});
    setPreflight(null);
    setBlockedSignature('');
  }

  async function load(preferId?: string) {
    setLoading(true);
    setError('');
    try {
      const next = await onLoadSchedules();
      setList(next);
      const wanted = next.schedules.find(item => item.id === (preferId || selectedId));
      const fallback = wanted || next.schedules[0];
      if (fallback) applySchedule(fallback);
      else if (!presetTargetId) {
        setSelectedId('');
        setDraft(EMPTY_DRAFT);
        setEditMode(false);
      }
    } catch {
      setError('读取定时计划失败，请检查本机服务后重试');
    } finally {
      setLoading(false);
    }
    await loadSkillRuns();
  }

  useEffect(() => {
    void load();
  }, []);

  // 全局 F5 / Ctrl+R：计划清单只存在于本页，所以由页面自己响应这次统一刷新。
  const loadRef = useRef(load);
  loadRef.current = load;
  useEffect(() => {
    const onRefreshPage = () => {
      onRefreshWorkflows();
      void loadRef.current();
    };
    window.addEventListener('himind:refresh-page', onRefreshPage);
    return () => window.removeEventListener('himind:refresh-page', onRefreshPage);
  }, [onRefreshWorkflows]);

  // 计划跑起来之后，「下次执行 / 上次结果 / 运行中」都会变，页面自己得跟上，
  // 否则打开着也看不出计划已经执行。静默刷新只替换快照，不碰右侧表单里的编辑内容。
  // 轮询周期跟着运行态走：只有真有计划在跑时才 15 秒一次，闲时 60 秒一次——
  // 等一条明天才跑的计划不需要每秒级心跳，需要立刻重取时用统一的 F5 / Ctrl+R。
  useEffect(() => {
    const timer = window.setInterval(() => {
      if (document.visibilityState === 'hidden') return;
      void onLoadSchedules()
        .then(next => setList(next))
        .catch(() => { /* 单次读取失败就先保留上一份快照 */ });
    }, hasLiveRuns ? 15000 : 60000);
    return () => window.clearInterval(timer);
  }, [onLoadSchedules, hasLiveRuns]);

  // 静默刷新换的是存储侧数据。选中的这条计划如果在存储里变了（例如另一处改了 Cron），
  // 而右侧表单没有未保存的改动，就让表单跟着走，避免左右两边显示两套值。
  useEffect(() => {
    const loaded = loadedRef.current;
    if (!loaded) return;
    if (editModeRef.current) return;
    const item = (list?.schedules || []).find(entry => entry.id === loaded.id);
    if (!item) return;
    if (scheduleSignature(item) === loaded.signature) return;
    if (JSON.stringify(draftRef.current) !== loaded.draft) return;
    applySchedule(item);
  }, [list]);

  useEffect(() => {
    if (draft.kind !== 'workflow' || !draft.targetId) {
      setPresets([]);
      setPresetsTarget('');
      return;
    }
    let disposed = false;
    void onLoadWorkflowPresets(draft.targetId)
      .then(result => {
        if (disposed) return;
        setPresets(result.presets || []);
        setPresetsTarget(draft.targetId);
      })
      .catch(() => {
        if (disposed) return;
        setPresets([]);
        setPresetsTarget('');
      });
    return () => { disposed = true; };
  }, [draft.kind, draft.targetId, onLoadWorkflowPresets]);

  // 从工作流页“加定时计划”跳过来时，预选目标并清空选中计划。
  useEffect(() => {
    if (!presetTargetId) return;
    setSelectedId('');
    setStoredId('');
    setEditMode(true);
    setParamsOpen(false);
    const fields = fieldsFor(presetTargetId);
    const form = initialFormValues(fields);
    setDraft(current => ({
      ...EMPTY_DRAFT,
      kind: 'workflow',
      targetId: presetTargetId,
      inputMode: fields.length ? 'form' : 'json',
      form,
      input: JSON.stringify(fields.length ? fieldsToInput(fields, form) : {}, null, 2),
    }));
    setFieldErrors({});
    setPreflight(null);
    setBlockedSignature('');
  }, [presetTargetId]);

  function beginNew() {
    setSelectedId('');
    setStoredId('');
    setError('');
    setEditMode(true);
    setParamsOpen(false);
    const targetId = draft.kind === 'skill'
      ? (skills[0]?.id || '')
      : (draft.targetId || workflows[0]?.package.id || '');
    const fields = draft.kind === 'workflow' ? fieldsFor(targetId) : [];
    const form = initialFormValues(fields);
    setDraft({
      ...EMPTY_DRAFT,
      kind: draft.kind,
      targetId,
      inputMode: inputModeFor(draft.kind, fields),
      form,
      input: JSON.stringify(fields.length ? fieldsToInput(fields, form) : {}, null, 2),
    });
    setFieldErrors({});
    setPreflight(null);
    setBlockedSignature('');
  }

  /**
   * 编辑已有计划：先按存储里的那条重新装载，再切到表单，避免带进上一次的半份草稿。
   * 引用预设的计划在存储里只留覆盖项，所以要在这里把「预设 + 覆盖项」合并后再填表单，
   * 否则用户点开编辑会看到工作区一片空白，以为这条计划根本没带参数。
   */
  function beginEdit() {
    if (!selected) return;
    applySchedule(selected);
    setEditMode(true);
    const preset = selected.preset_id ? presets.find(item => item.id === selected.preset_id) || null : null;
    if (!preset) return;
    const resolved = resolvedInput(selected.input, preset);
    const fields = fieldsFor(selected.target_id);
    setDraft(current => ({
      ...current,
      input: JSON.stringify(resolved, null, 2),
      form: fields.length ? jsonToFormValues(fields, resolved) : {},
      entrypoint: current.entrypoint || preset.entrypoint || '',
      exitpoint: current.exitpoint || preset.exitpoint || '',
    }));
  }

  /** 取消编辑：草稿直接丢掉，回到存储里那条的只读详情。 */
  function cancelEdit() {
    setError('');
    setFieldErrors({});
    setPreflight(null);
    setBlockedSignature('');
    if (selected) {
      applySchedule(selected);
      return;
    }
    setSelectedId('');
    setStoredId('');
    setDraft(EMPTY_DRAFT);
    setEditMode(false);
    setParamsOpen(false);
  }

  /** 目录字段走系统选择器，省掉手输路径：工作区是参数的一部分，改它不该靠打字。 */
  async function pickDirectory(fieldId: string) {
    try {
      const path = await onPickDirectory();
      if (!path) return;
      setDraft(current => ({ ...current, form: { ...current.form, [fieldId]: path } }));
      setFieldErrors(current => {
        if (!current[fieldId]) return current;
        const next = { ...current };
        delete next[fieldId];
        return next;
      });
      setPreflight(null);
    } catch {
      setError('选择目录失败');
    }
  }

  function buildInput(): Record<string, unknown> {
    if (draft.kind === 'workflow' && draft.inputMode === 'form' && workflowFields.length) {
      const missing = workflowFields.find(field => field.required && !String(draft.form[field.id] ?? '').trim());
      if (missing) {
        setFieldErrors({ [missing.id]: `${missing.label}不能为空` });
        throw new Error(`${missing.label}不能为空`);
      }
      const invalid = workflowFields
        .map(field => ({ field, invalidValue: invalidOptionValue(field, draft.form[field.id]) }))
        .find(item => item.invalidValue);
      if (invalid) {
        setFieldErrors({ [invalid.field.id]: `${invalid.field.label}不支持「${invalid.invalidValue}」` });
        throw new Error(`${invalid.field.label}不支持「${invalid.invalidValue}」`);
      }
      return fieldsToInput(workflowFields, draft.form);
    }
    try {
      const parsed = draft.input.trim() ? JSON.parse(draft.input) : {};
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('not-object');
      return parsed as Record<string, unknown>;
    } catch {
      throw new Error('运行输入不是合法 JSON 对象');
    }
  }

  async function runWorkflowPreflight(input: Record<string, unknown>) {
    if (draft.kind !== 'workflow' || !draft.targetId) return true;
    setPreflightBusy(true);
    try {
      const preflightInput: Record<string, unknown> = { ...input };
      if (draft.entrypoint.trim() || draft.exitpoint.trim()) {
        preflightInput.execution = { entrypoint: draft.entrypoint.trim(), exitpoint: draft.exitpoint.trim() };
      }
      const report = await onPreflightWorkflow(draft.targetId, preflightInput);
      setPreflight(report);
      if (!report.ready) {
        setError(preflightNote(report, '运行前检查未通过'));
        return false;
      }
      return true;
    } catch {
      setPreflight(null);
      setError('运行前检查失败，请检查工作流依赖和连接状态');
      return false;
    } finally {
      setPreflightBusy(false);
    }
  }

  async function save(skipPreflight = false) {
    const name = draft.id.trim();
    const nameError = scheduleNameError(name);
    if (nameError) {
      setError(nameError);
      return;
    }
    // 改名会换成另一个句柄。撞上已有计划时直接拦下：否则保存会静默改写那条计划。
    const clash = name && name !== storedId
      ? (list?.schedules || []).find(item => item.id === name)
      : undefined;
    if (clash) {
      setError(`已经有一条叫「${name}」的计划，换个名字或留空沿用原来的标识`);
      return;
    }
    let parsed: Record<string, unknown>;
    try {
      parsed = buildInput();
    } catch (inputError) {
      setError(inputError instanceof Error ? inputError.message : '请检查运行输入');
      return;
    }
    if (!draft.targetId) {
      setError('请选择要定时运行的目标');
      return;
    }
    if (cronError) {
      setError(cronError);
      return;
    }
    if (draft.kind === 'workflow' && !skipPreflight && !(await runWorkflowPreflight(parsed))) {
      // 运行条件没就绪不该阻止「先把计划建起来」：这里放行一次，但要用户明确再点一下，
      // 并且只对"这份没改动的草稿"有效，不会悄无声息地存下一条跑不起来的计划。
      setBlockedSignature(draftSignature);
      return;
    }
    setBlockedSignature('');
    setBusy('save');
    setError('');
    try {
      // 名称留空 = 沿用这条计划原来的句柄；新建计划才现生成一个不冲突的。
      const saveId = name
        || storedId
        || autoScheduleId(draft.targetId, new Set((list?.schedules || []).map(item => item.id)));
      // 引用了预设就只存覆盖项：预设里改了工作区，这条计划下次到点自动用新值，
      // 不必回来逐条计划改一遍。没引用预设时才存整份快照。
      const preset = draft.kind === 'workflow' ? draftPreset : null;
      const savedId = await onSaveSchedule({
        id: saveId,
        kind: draft.kind,
        target_id: draft.targetId,
        preset_id: draft.kind === 'workflow' ? (draft.presetId || undefined) : undefined,
        cron: draft.cron.trim(),
        input: overrideInput(parsed, preset),
        execution: {
          entrypoint: overrideEndpoint(draft.entrypoint, preset?.entrypoint || ''),
          exitpoint: overrideEndpoint(draft.exitpoint, preset?.exitpoint || ''),
        },
        enabled: draft.enabled,
      });
      // 改名等于换句柄，旧记录不清掉会留下两条同时在跑的重复计划。
      if (name && storedId && name !== storedId) {
        try {
          await onDeleteSchedule(storedId, { silent: true });
        } catch {
          /* 旧记录清理失败不影响这次保存，列表里能看出来再删一次即可 */
        }
      }
      const nextStoredId = savedId || saveId || '';
      setStoredId(nextStoredId);
      await load(nextStoredId);
    } catch (saveError) {
      setError(saveError instanceof Error && saveError.message ? saveError.message : '保存定时计划失败，请确认目标是否已启用或安装');
    } finally {
      setBusy('');
    }
  }

  async function toggleSchedule(item: Schedule) {
    setBusy(`toggle:${item.id}`);
    setError('');
    try {
      await onSaveSchedule({
        id: item.id,
        kind: item.kind,
        target_id: item.target_id,
        preset_id: item.preset_id || undefined,
        cron: item.cron,
        input: item.input || {},
        execution: item.execution,
        enabled: !item.enabled,
      });
      await load(item.id);
    } catch (toggleError) {
      setError(toggleError instanceof Error && toggleError.message ? `更新计划状态失败：${toggleError.message}` : '更新计划状态失败');
    } finally {
      setBusy('');
    }
  }

  async function remove(id: string) {
    setBusy(`delete:${id}`);
    setError('');
    try {
      await onDeleteSchedule(id);
      if (selectedId === id) setSelectedId('');
      await load();
    } catch {
      // 失败提示由外层统一给（toast），这里不再叠一条页面内错误。
    } finally {
      setBusy('');
    }
  }

  async function runNow(item: Schedule) {
    setBusy(`run:${item.id}`);
    setError('');
    try {
      if (item.kind === 'skill') {
        await onRunSkill(item.target_id, { ...(item.input || {}) });
        await loadSkillRuns();
        return;
      }
      const preset = await ensurePreset(item.target_id, item.preset_id || '');
      const input: Record<string, unknown> = resolvedInput(item.input, preset);
      // 入口/出口同样先看计划有没有显式覆盖，没有再回落到预设。
      const entrypoint = (item.execution?.entrypoint || '').trim() || (preset?.entrypoint || '').trim();
      const exitpoint = (item.execution?.exitpoint || '').trim() || (preset?.exitpoint || '').trim();
      if (entrypoint || exitpoint) input.execution = { entrypoint, exitpoint };
      await onRunTarget(item.kind, item.target_id, input);
    } catch (runError) {
      setError(runError instanceof Error ? runError.message : '立即运行失败，请查看运行记录');
    } finally {
      setBusy('');
    }
  }

  /**
   * 拿这条计划引用的预设：已经在内存里的直接复用，否则按计划自己的目标工作流读一次。
   * 立即运行要按「预设 + 覆盖项」的合并结果跑，不能只发计划身上那份覆盖项。
   */
  async function ensurePreset(targetId: string, presetId: string) {
    if (!presetId) return null;
    const loaded = presets.find(item => item.id === presetId && item.workflow_id === targetId);
    if (loaded) return loaded;
    try {
      const result = await onLoadWorkflowPresets(targetId);
      const items = result.presets || [];
      setPresets(items);
      setPresetsTarget(targetId);
      return items.find(item => item.id === presetId) || null;
    } catch {
      return null;
    }
  }

  function selectPreset(presetId: string) {
    if (!presetId) {
      setDraft(current => ({ ...current, presetId: '' }));
      // 不用预设就代表参数要自己填，直接把参数区展开。
      setParamsOpen(true);
      setPreflight(null);
      return;
    }
    const preset = presets.find(item => item.id === presetId);
    if (!preset) return;
    // 预设已经把工作区等参数带齐了，收起参数区，只留「到点就跑」这一件事。
    setParamsOpen(false);
    const fields = fieldsFor(preset.workflow_id);
    const input = preset.input || {};
    setDraft(current => ({
      ...current,
      presetId,
      targetId: preset.workflow_id,
      entrypoint: preset.entrypoint || '',
      exitpoint: preset.exitpoint || '',
      input: JSON.stringify(input, null, 2),
      inputMode: fields.length ? 'form' : 'json',
      form: fields.length ? jsonToFormValues(fields, input) : {},
    }));
    setFieldErrors({});
    setPreflight(null);
  }

  function switchInputMode(mode: 'form' | 'json') {
    if (mode === draft.inputMode) return;
    if (mode === 'json') {
      try {
        const input = fieldsToInput(workflowFields, draft.form);
        setDraft(current => ({ ...current, inputMode: 'json', input: JSON.stringify(input, null, 2) }));
        setFieldErrors({});
      } catch {
        setError('结构化输入暂时无法转换为 JSON');
      }
      return;
    }
    try {
      const parsed = draft.input.trim() ? JSON.parse(draft.input) : {};
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('not-object');
      setDraft(current => ({ ...current, inputMode: 'form', form: jsonToFormValues(workflowFields, parsed) }));
      setFieldErrors({});
    } catch {
      setError('JSON 需要是有效对象，才能切换回结构化输入');
    }
  }

  function setWorkflowField(field: WorkflowStartField, value: unknown) {
    setDraft(current => ({ ...current, form: { ...current.form, [field.id]: value } }));
    setFieldErrors(current => {
      if (!current[field.id]) return current;
      const next = { ...current };
      delete next[field.id];
      return next;
    });
    setPreflight(null);
  }

  /** 没有表单字段的工作流（以及技能）仍然要能填参数，共用同一个 JSON 输入块。 */
  function jsonInputField(hint: string, placeholder: string) {
    return (
      <label className="workflow-start-field-wide">
        <span>运行输入（JSON）{hint ? ` · ${hint}` : ''}</span>
        <textarea
          value={draft.input}
          spellCheck={false}
          rows={6}
          placeholder={placeholder}
          onChange={event => { setDraft(current => ({ ...current, input: event.target.value })); setPreflight(null); }}
        />
      </label>
    );
  }

  return (
    <div className="workflow-page">
      <PageHeader title="定时计划" />
      {showSummary ? (
        <section className="workflow-summary" aria-label="定时计划概览">
          <div><CalendarClock size={18} /><span><small>任务总数</small><strong>{totalSchedules}</strong></span></div>
          <div className={enabledCount ? '' : 'attention'}><Power size={18} /><span><small>已启用</small><strong>{enabledCount}</strong></span></div>
          <div><Pause size={18} /><span><small>已停用</small><strong>{pausedCount}</strong></span></div>
          <div className={failedCount ? 'attention' : ''}><CircleAlert size={18} /><span><small>上次失败</small><strong>{failedCount}</strong></span></div>
        </section>
      ) : null}
      <div className="workflow-layout">
        <section className="card workflow-list-panel">
          <div className="card-header">
            <strong>全部计划</strong>
            <span className="workflow-library-actions workflow-library-actions-primary">
              <span className="section-count">{filteredSchedules.length !== totalSchedules ? `${filteredSchedules.length} / ${totalSchedules} 条` : `${totalSchedules} 条`}</span>
              <button type="button" className="btn btn-primary" onClick={beginNew}><CalendarClock size={14} />新建定时计划</button>
            </span>
          </div>
          {showListTools ? (
            <div className="schedule-list-tools" role="search">
              <label className="schedule-search">
                <Search size={14} aria-hidden="true" />
                <input value={scheduleSearch} placeholder="搜索计划" aria-label="搜索定时计划" onChange={event => setScheduleSearch(event.target.value)} />
              </label>
              <select value={scheduleStatusFilter} aria-label="筛选定时计划状态" onChange={event => setScheduleStatusFilter(event.target.value as typeof scheduleStatusFilter)}>
                <option value="all">全部状态</option>
                <option value="enabled">已启用</option>
                <option value="paused">已停用</option>
                <option value="failed">上次失败</option>
              </select>
            </div>
          ) : null}
          <div className="workflow-list">
            {filteredSchedules.map(item => {
              const liveState = liveRuns.get(item.last_run_id);
              // 正在跑的计划，「下次」暂时没有意义，那一行改说「已运行多久」——它是走动的，最能说明现在是活的。
              const liveSeconds = liveState && liveState.startedAt !== null
                ? Math.max(0, Math.floor(nowMs / 1000) - liveState.startedAt)
                : null;
              return (
              <div className={`schedule-list-item${selectedId === item.id ? ' active' : ''}${item.enabled ? '' : ' is-disabled'}`} key={item.id}>
                <button
                  type="button"
                  className={liveState ? 'is-live' : ''}
                  onClick={() => applySchedule(item)}
                >
                  <span className="workflow-list-mark"><CalendarClock size={16} /></span>
                  <span>
                    {/* 标题是「跑什么」，句柄与周期并到副行：四行文字压到三行，行高也跟着收回来。 */}
                    <strong title={item.id}>{planDisplayName(item, targetName)}</strong>
                    <small>
                      {[planHandleLabel(item, targetName), targetLabel(item.kind), item.preset_id ? '使用方案' : '']
                        .filter(Boolean)
                        .join(' · ')}
                    </small>
                    {/* 停用后 next_run_at 不再推进（scheduler 的 due_at 直接返回 false），
                        继续写「下次 …」先给出一个不会发生的时刻，过期后还会一直停在过去。
                        停用行改说状态 + 周期，「停用」这个词也就不必再占一枚胶囊。 */}
                    <small>{liveState
                      ? (liveSeconds !== null ? `已运行 ${formatElapsedCn(liveSeconds)}` : liveState.label)
                      : item.enabled
                        ? `${cronDescription(item.cron) || item.cron} · 下次 ${formatCompactStamp(item.next_run_at, nowMs)}`
                        : `已停用 · ${cronDescription(item.cron) || item.cron}`}</small>
                  </span>
                  {/* 默认启用是常态，不用每行再挂一个「已启用」：异常状态才值得占一格。
                      「已停用」也不再挂胶囊——这一行只有 151–177px 放文字，62px 的胶囊
                      会把句柄和周期两行一起挤成省略号（实测 1280 下 177→115，1024 下 151→89），
                      而且和开关的「关」重复说了同一件事。状态词已由上面第三行承载，
                      胶囊只留给「运行中 / 上次失败」这类要抢注意的态。 */}
                  {liveState
                    ? <Pill kind="live">{liveState.label}</Pill>
                    : item.last_status === 'failed'
                      ? <Pill kind="danger">上次失败</Pill>
                      : null}
                </button>
                {/* 行尾开关是计划唯一的启停控件。
                    原来这里是一枚电源图标按钮：启用态「主色底 + 主色图标」，和同一行的「已启用」
                    胶囊撞成两个绿色标记——看着像状态，其实是动作。换成开关后状态和动作是同一个
                    控件，位置固定。详情底部那枚「停用计划 / 启用计划」按钮也一并去掉：同一屏左右
                    各一枚说的是同一个布尔值，改一处另一处得跟着动才算对。
                    编辑态例外：表单里的「启用该任务」是这次保存的事务内容，开关让位，否则保存会
                    把手改的这一下静默盖回去。 */}
                <label
                  className="toggle compact schedule-quick-toggle"
                  title={editMode && storedId === item.id
                    ? '正在编辑这条计划，状态以表单里的「启用该任务」为准'
                    : item.enabled
                      ? '停用计划（停用后不再按周期自动运行）'
                      : '启用计划（按周期自动运行）'}
                >
                  <input
                    type="checkbox"
                    checked={item.enabled}
                    disabled={Boolean(busy) || (editMode && storedId === item.id)}
                    aria-label={`${item.enabled ? '停用' : '启用'}计划 ${item.id}`}
                    onChange={() => void toggleSchedule(item)}
                  />
                  <span className="slider" />
                </label>
              </div>
              );
            })}
            {!loading && (list?.schedules || []).length > 0 && filteredSchedules.length === 0 ? (
              <div className="workflow-library-empty">
                <EmptyState icon={Search} title="没有匹配的定时计划" text="换一个计划名称、工作流名称或状态试试。" />
                <button type="button" className="btn" onClick={() => { setScheduleSearch(''); setScheduleStatusFilter('all'); }}>清除筛选</button>
              </div>
            ) : null}
            {!loading && (list?.schedules || []).length === 0 ? (
              <div className="workflow-library-empty">
                {/* 新建入口就在上面那一行，空状态再放一个同名主按钮只会让人以为有两个动作。 */}
                <EmptyState icon={CalendarClock} title="还没有定时计划" text="点上方「新建定时计划」，之后会按计划自动运行。" />
              </div>
            ) : null}
          </div>
        </section>
        <section className={`card workflow-detail-panel schedule-detail-panel${editMode ? ' is-editing' : ''}`}>
          {error ? <div className="workflow-inline-error">{error}</div> : null}
          <div className="workflow-detail-head">
            <div>
              <span>{editMode ? '编辑定时计划' : '计划详情'}</span>
              {/* 标题就是这条计划在跑的目标；计划句柄放进 title，排查时悬停可见。 */}
              {/* 状态胶囊跟标题同行（与工作流详情同一处）：动作区改放按钮之后，
                  胶囊留在右侧会和按钮挤成同一簇。 */}
              <div className="workflow-title-line">
                <h2 title={!editMode && selected ? selected.id : undefined}>{editMode
                  ? (draft.targetId ? targetName(draft.kind, draft.targetId) : '新建定时计划')
                  : selected ? planDisplayName(selected, targetName) : '未选择计划'}</h2>
                {/* 与列表同一套取舍：默认启用是常态，只把需要留意的状态挂成胶囊。 */}
                {editMode
                  ? <Pill kind="neutral">编辑中</Pill>
                  : selected && !selected.enabled
                    ? <Pill kind="neutral">已停用</Pill>
                    : selected && selected.last_status === 'failed'
                      ? <Pill kind="danger">上次失败</Pill>
                      : null}
              </div>
              {/* 编辑态跟着表单里的 Cron 走：改完表达式在这里就能看到「什么时候跑」。
                  只剩一句「按时区运行」等于没告诉用户任何事。 */}
              <p>{editMode
                ? [
                    cronDescription(draft.cron) || draft.cron.trim(),
                    `时区 ${formatTimezone(list?.timezone)}`,
                  ].filter(Boolean).join(' · ') || `时区 ${formatTimezone(list?.timezone)}`
                : selected
                  ? [
                      cronDescription(selected.cron) || selected.cron,
                      `时区 ${formatTimezone(list?.timezone)}`,
                    ].filter(Boolean).join(' · ')
                  : `时区 ${formatTimezone(list?.timezone)}`}</p>
            </div>
            <div className="workflow-detail-actions">
              {/* 对象级动作跟对象走，位置与技能、插件、工作流一致：主操作 → 次要 → 危险，
                  同一簇右对齐。
                  启停不在这里：左侧列表行尾那枚开关已经是这个状态的唯一控件。
                  删除和启停不同，是真的会把这条计划从存储里拿掉，所以按下先过一遍确认弹窗。 */}
              {!editMode && selected ? (
                <>
                  <button type="button" className="btn btn-primary" title="按现在这套参数立即跑一次" disabled={Boolean(busy)} onClick={() => void runNow(selected)}>{busy.startsWith('run:') ? <BusyIndicator size={14} /> : <Play size={14} />}{busy.startsWith('run:') ? '启动中' : '立即运行'}</button>
                  <button type="button" className="btn" title="改这套计划的周期或参数" disabled={Boolean(busy)} onClick={beginEdit}><Pencil size={14} />编辑</button>
                  <button
                    type="button"
                    className="btn btn-danger-quiet"
                    title="删除这条计划"
                    disabled={Boolean(busy)}
                    onClick={() => {
                      setPendingConfirm({
                        title: '确认删除定时计划？',
                        description: `删除「${planDisplayName(selected, targetName)}」后它不再按周期运行，运行记录里已有的结果会保留。`,
                        confirmText: '确认删除',
                        run: () => void remove(selected.id),
                      });
                    }}
                  >
                    <Trash2 size={14} />{busy.startsWith('delete:') ? '删除中' : '删除计划'}
                  </button>
                </>
              ) : null}
            </div>
          </div>
          <div className="schedule-detail-scroll">
          {!editMode && !selected ? (
            <div className="workflow-library-empty">
              <EmptyState
                icon={CalendarClock}
                /* 面板抬头已经写过「计划详情」，这里再写一次就是同一句话上下两遍。 */
                title={!loading && totalSchedules === 0 ? '还没有定时计划' : '还没有选中计划'}
                text={!loading && totalSchedules === 0 ? '创建计划后，这里显示下次运行时间和最近一次结果。' : '从左侧列表选一条查看状态。'}
              />
            </div>
          ) : null}
          {!editMode && selectedLiveRun ? (
            <div className="schedule-live-panel" role="status">
              <span className="workflow-run-pulse" aria-hidden="true" />
              <div className="schedule-live-text">
                <strong>{liveRuns.get(selectedLiveRun.run.run_id)?.label || '运行中'}</strong>
                <small>
                  {selectedLiveRun.current_step_title || selectedLiveRun.business_stage || '已交给运行中心'}
                  {' · '}
                  {formatRelativeStamp(selectedLiveRun.run.updated_at, nowMs)}更新
                </small>
              </div>
              <span className="schedule-live-run">{shortRunId(selectedLiveRun.run.run_id)}</span>
            </div>
          ) : null}
          <div className="workflow-start-form schedule-form">
            {editMode ? <>
            <section>
              <h3>运行什么</h3>
              <div className="workflow-start-fields">
                <label>
                  <span>任务名称（留空自动生成）</span>
                  <input value={draft.id} placeholder="daily-tech-radar" spellCheck={false} onChange={event => setDraft(current => ({ ...current, id: event.target.value.trimStart() }))} />
                  <small className="workflow-start-hint">
                    {storedId && !draft.id.trim()
                      ? '留空就继续用这条计划原来的标识，不会多出一条。'
                      : draft.id.trim()
                        ? '只能用字母、数字、点、横线和下划线；改名会替换原标识。'
                        : '留空按目标自动生成标识，只能用字母、数字、点、横线和下划线。'}
                  </small>
                </label>
                <label>
                  <span>目标类型</span>
                  <select value={draft.kind} onChange={event => {
                    const kind = event.target.value;
                    const targetId = kind === 'skill' ? (skills[0]?.id || '') : (workflows[0]?.package.id || '');
                    const fields = kind === 'workflow' ? fieldsFor(targetId) : [];
                    const form = initialFormValues(fields);
                    setDraft(current => ({ ...current, kind, targetId, presetId: '', inputMode: inputModeFor(kind, fields), form, input: JSON.stringify(fields.length ? fieldsToInput(fields, form) : {}, null, 2) }));
                    setFieldErrors({});
                    setPreflight(null);
                  }}>
                    {(list?.target_kinds || ['workflow']).map(kind => <option key={kind} value={kind}>{targetLabel(kind)}</option>)}
                  </select>
                </label>
                <label className="workflow-start-field-wide">
                  <span>目标</span>
                  {draft.kind === 'workflow' ? (
                    <select value={draft.targetId} onChange={event => {
                      const targetId = event.target.value;
                      const fields = fieldsFor(targetId);
                      const form = initialFormValues(fields);
                      setDraft(current => ({ ...current, targetId, presetId: '', inputMode: fields.length ? 'form' : 'json', form, input: JSON.stringify(fields.length ? fieldsToInput(fields, form) : {}, null, 2) }));
                      setFieldErrors({});
                      setPreflight(null);
                    }}>
                      <option value="">请选择工作流</option>
                      {workflows.map(item => <option key={item.package.id} value={item.package.id}>{item.package.name}</option>)}
                    </select>
                  ) : draft.kind === 'skill' ? (
                    <select value={draft.targetId} onChange={event => { setDraft(current => ({ ...current, targetId: event.target.value })); setPreflight(null); }}>
                      <option value="">请选择技能</option>
                      {skills.map(item => <option key={item.id} value={item.id}>{item.name} · v{item.version}</option>)}
                    </select>
                  ) : (
                    <input value={draft.targetId} spellCheck={false} onChange={event => setDraft(current => ({ ...current, targetId: event.target.value }))} />
                  )}
                </label>
                {draft.kind === 'skill' && skills.length === 0 ? (
                  <div className="workflow-start-empty workflow-start-field-wide">
                    还没有已安装的技能。<button type="button" className="btn" onClick={onOpenExtensions}><Store size={13} />去市场安装</button>
                  </div>
                ) : null}
                {draft.kind === 'workflow' && workflows.length === 0 ? (
                  <div className="workflow-start-empty workflow-start-field-wide">
                    还没有可用的工作流。<button type="button" className="btn" onClick={onOpenExtensions}><Store size={13} />去市场安装</button>
                  </div>
                ) : null}
                {draft.kind === 'workflow' ? (
                  <label>
                    <span>启动方案</span>
                    <select value={draft.presetId} onChange={event => selectPreset(event.target.value)}>
                      <option value="">不用方案（保存当前参数）</option>
                      {presets.map(preset => <option key={preset.id} value={preset.id}>{preset.label || preset.id}</option>)}
                    </select>
                    <small className="workflow-start-hint">
                      {presets.length ? '方案自带工作区等参数，到点直接按这套方案运行。' : '这个工作流还没有方案，可以先在工作流页把常用参数存成方案。'}
                    </small>
                  </label>
                ) : null}
              </div>
            </section>
            {draft.kind === 'workflow' ? (
              <section>
                <div className="workflow-start-params-head">
                  <div>
                    <strong>{draftPresetLabel ? `参数来自方案「${draftPresetLabel}」` : '启动参数'}</strong>
                    <small>{draftPresetLabel ? '这里的改动只影响这条计划，不会写回方案。' : '工作区等参数会跟计划一起保存，到点直接按这套参数运行。'}</small>
                  </div>
                  <button type="button" className="btn" onClick={() => setParamsOpen(value => !value)}>{paramsExpanded ? '收起参数' : '调整参数'}</button>
                </div>
                {paramsExpanded ? (
                <div className="workflow-start-fields">
                  {entrypoints.length > 1 || Boolean(draft.entrypoint) ? (
                    <label>
                      <span>入口（多入口）</span>
                      <select value={draft.entrypoint} onChange={event => setDraft(current => ({ ...current, entrypoint: event.target.value }))}>
                        <option value="">按工作流默认</option>
                        {entrypoints.map(entry => <option key={entry.id} value={entry.id}>{entry.label || entry.id}</option>)}
                      </select>
                    </label>
                  ) : null}
                  {exits.length > 1 || Boolean(draft.exitpoint) ? (
                    <label>
                      <span>出口（多出口）</span>
                      <select value={draft.exitpoint} onChange={event => setDraft(current => ({ ...current, exitpoint: event.target.value }))}>
                        <option value="">按工作流默认</option>
                        {exits.map(exit => <option key={exit.id} value={exit.id}>{exit.label || exit.id}</option>)}
                      </select>
                    </label>
                  ) : null}
                {workflowFields.length ? (
                  <div className="workflow-start-field-wide schedule-workflow-input">
                    <div className="workflow-start-mode segmented-control" role="group" aria-label="工作流输入模式">
                      <button type="button" className={draft.inputMode === 'form' ? 'active' : ''} onClick={() => switchInputMode('form')}>结构化输入</button>
                      <button type="button" className={draft.inputMode === 'json' ? 'active' : ''} onClick={() => switchInputMode('json')}>JSON</button>
                    </div>
                    {draft.inputMode === 'form' ? (
                      <div className="workflow-start-form schedule-workflow-fields">
                        {targetWorkflow?.view?.sections.map(section => (
                          <section key={section.id}>
                            <h3>{section.title}</h3>
                            <div className="workflow-start-fields">
                              {(section.fields || []).map(rawField => {
                                const field = normalizeStartField(rawField);
                                const value = draft.form[field.id];
                                const fieldError = fieldErrors[field.id];
                                const label = <span>{field.label}{field.required ? <em>必需</em> : null}</span>;
                                const footer = <>{fieldError ? <small className="workflow-start-field-error">{fieldError}</small> : null}{field.hint ? <small className="workflow-start-hint">{field.hint}</small> : null}</>;
                                const fieldClass = field.type === 'textarea' || field.type === 'list' || field.type === 'json' || field.span === 'full' ? 'workflow-start-field-wide' : '';
                                if (field.type === 'boolean') {
                                  return <label key={field.id} className="workflow-start-toggle"><input type="checkbox" checked={Boolean(value)} onChange={event => setWorkflowField(field, event.target.checked)} /><span>{field.label}</span></label>;
                                }
                                if (field.type === 'select') {
                                  return <label key={field.id} className={fieldClass}>{label}<select value={String(value ?? '')} onChange={event => setWorkflowField(field, event.target.value)}><option value="">请选择</option>{field.optionEntries.map(option => <option key={option.value} value={option.value}>{option.label}</option>)}</select>{footer}</label>;
                                }
                                if (field.type === 'textarea' || field.type === 'list' || field.type === 'json') {
                                  return <label key={field.id} className={fieldClass}>{label}<textarea rows={field.type === 'list' ? 3 : 4} value={String(value ?? '')} placeholder={field.placeholder || (field.type === 'list' ? '每行一项' : '')} onChange={event => setWorkflowField(field, event.target.value)} />{footer}</label>;
                                }
                                // 目录字段和「工作流」页保持一致给系统选择器：工作区是参数的一部分，
                                // 换个项目不该靠手打路径。
                                const directoryField = field.picker === 'directory'
                                  || field.id === 'workspace_root'
                                  || field.id === 'source_root'
                                  || field.id === 'project_root';
                                if (directoryField) {
                                  return (
                                    <label key={field.id} className={fieldClass}>
                                      {label}
                                      <div className="workflow-path-picker">
                                        <input type="text" value={String(value ?? '')} placeholder={field.placeholder} aria-invalid={Boolean(fieldError)} onChange={event => setWorkflowField(field, event.target.value)} />
                                        <button type="button" className="btn btn-icon" title="选择目录" aria-label={`选择${field.label}`} onClick={() => void pickDirectory(field.id)}><FolderOpen size={14} /></button>
                                      </div>
                                      {footer}
                                    </label>
                                  );
                                }
                                return <label key={field.id} className={fieldClass}>{label}<input type={field.type === 'number' ? 'number' : 'text'} value={String(value ?? '')} placeholder={field.placeholder} aria-invalid={Boolean(fieldError)} onChange={event => setWorkflowField(field, event.target.value)} />{footer}</label>;
                              })}
                            </div>
                          </section>
                        ))}
                      </div>
                    ) : (
                      <textarea className="workflow-start-json" value={draft.input} spellCheck={false} rows={8} onChange={event => { setDraft(current => ({ ...current, input: event.target.value })); setPreflight(null); }} />
                    )}
                  </div>
                ) : jsonInputField('', '{}')}
                </div>
                ) : (
                  <div className="workflow-start-summary">
                    <div><span>工作区</span><strong title={draftWorkspace}>{draftWorkspace || '未指定'}</strong></div>
                    {draft.entrypoint ? <div><span>入口</span><strong>{draft.entrypoint}</strong></div> : null}
                    {draft.exitpoint ? <div><span>出口</span><strong>{draft.exitpoint}</strong></div> : null}
                    <div><span>参数项</span><strong>{filledParamCount}/{workflowFields.length} 已填</strong></div>
                  </div>
                )}
              </section>
            ) : (
              <section>
                <h3>运行输入</h3>
                <div className="workflow-start-fields">
                  {jsonInputField('技能需要 task，可加 workspace_root / timeout_seconds', '{"task":"本次要让技能做什么"}')}
                </div>
              </section>
            )}
            <section>
              <h3>什么时候跑</h3>
              <div className="workflow-start-fields">
                <label className="workflow-start-field-wide schedule-cron-field">
                  <span>运行计划（分 时 日 月 周）</span>
                  <input value={draft.cron} placeholder="0 9 * * *" spellCheck={false} aria-invalid={Boolean(cronError)} onChange={event => setDraft(current => ({ ...current, cron: event.target.value }))} />
                  <small className={cronError ? 'workflow-start-field-error' : 'workflow-start-hint'}>
                    {cronError || cronDescription(draft.cron) || '格式：分 时 日 月 周 · 例如每天 09:00 可用 0 9 * * *'}
                  </small>
                </label>
                <div className="workflow-start-field-wide schedule-presets">
                  {CRON_PRESETS.map(preset => (
                    <button type="button" key={preset.cron} className="btn" onClick={() => setDraft(current => ({ ...current, cron: preset.cron }))}>{preset.label}</button>
                  ))}
                </div>
              </div>
            </section>
            <section>
              <h3>其他</h3>
              <div className="workflow-start-fields">
                <label className="workflow-start-toggle">
                  <input type="checkbox" checked={draft.enabled} onChange={event => setDraft(current => ({ ...current, enabled: event.target.checked }))} />
                  <span>启用该任务</span>
                </label>
              </div>
            </section>
            </> : null}
            {editMode && draft.kind === 'workflow' ? (
              <section className={`workflow-preflight schedule-preflight${preflight?.ready ? ' ready' : preflight ? ' blocked' : ''}`}>
                <div className="workflow-preflight-head">
                  <span><strong>{preflight ? (preflight.ready ? '运行前检查通过' : '运行前检查未通过') : '保存前检查运行条件'}</strong><small>{preflight ? (preflight.ready ? '保存后将按当前参数运行。' : (blockerHeadline(preflight) || '请先处理阻塞项。')) : '提前检查依赖和凭据，避免任务执行失败。'}</small></span>
                  <button type="button" className="btn" disabled={preflightBusy || !draft.targetId} onClick={() => { try { const input = buildInput(); void runWorkflowPreflight(input); } catch (inputError) { setError(inputError instanceof Error ? inputError.message : '请检查工作流输入'); } }}>{preflightBusy ? '检查中' : '运行前检查'}</button>
                </div>
                {preflight ? <div className="workflow-preflight-grid"><span>功能 {preflight.capabilities.filter(item => item.available).length}/{preflight.capabilities.length}</span><span>技能 {preflight.skills.filter(item => item.available).length}/{preflight.skills.length}</span><span>运行环境 {preflight.runtimes.filter(item => item.available).length}/{preflight.runtimes.length}</span><span>连接 {preflight.connectors.filter(item => item.available && item.health_status === 'passed').length}/{preflight.connectors.length}</span></div> : null}
                {preflight && (preflight.blockers.length || preflight.warnings.length) ? (
                  <details className="schedule-preflight-details" open={!preflight.ready}>
                    <summary>{preflight.blockers.length ? `${preflight.blockers.length} 个阻塞项` : `${preflight.warnings.length} 条提示`}</summary>
                    {/* 阻塞项按「中文结论 + 原始原因」展示：原始报错留给排查，前面一行得先让人看懂。 */}
                    {blockerDiagnostics(preflight).map(item => (
                      <p key={`blocker:${item.code}:${item.message}`}><strong>阻塞</strong>{blockerLead(item.code) || item.code}{item.message && item.message !== blockerLead(item.code) ? <em>（{item.message}）</em> : null}</p>
                    ))}
                    {blockerDiagnostics(preflight).length === 0 ? preflight.blockers.map(message => <p key={`blocker:${message}`}><strong>阻塞</strong>{message}</p>) : null}
                    {preflight.warnings.map(message => <p key={`warning:${message}`}><strong>提示</strong>{message}</p>)}
                  </details>
                ) : null}
              </section>
            ) : null}
            {!editMode && selected ? (
              <section className="workflow-section">
                <div className="workflow-section-title"><strong>这条计划</strong></div>
                <div className="schedule-facts">
                  <div>
                    <span>运行周期</span>
                    <strong>{cronDescription(selected.cron) || selected.cron}</strong>
                    {cronDescription(selected.cron) ? <small>{selected.cron}</small> : null}
                  </div>
                  <div><span>运行目标</span><strong>{targetLabel(selected.kind)} · {targetName(selected.kind, selected.target_id)}</strong></div>
                  {/*
                    这一格只回答「这套参数从哪来」：没有引用方案时不成立——
                    写一句「本计划自带参数」既没说参数是什么，又占掉一整格，不如不出这一格。
                  */}
                  {selectedPreset ? (
                    <div>
                      <span>启动方案</span>
                      <strong>{selectedPreset.label || selectedPreset.id}</strong>
                      <small
                        className="schedule-fact-note"
                        title={selectedOverrideCount
                          ? `工作区等参数跟随方案，改方案即生效；这条计划另外覆盖了 ${selectedOverrideCount} 项`
                          : '工作区等参数跟随方案，改方案即生效'}
                      >{selectedOverrideCount
                        ? `参数跟随方案，另有 ${selectedOverrideCount} 项覆盖`
                        : '参数跟随方案，改方案即生效'}</small>
                    </div>
                  ) : null}
                  {selectedWorkspace ? <div><span>工作区</span><strong title={selectedWorkspace}>{tailPath(selectedWorkspace)}</strong></div> : null}
                </div>
                {selectedPresetMissing ? (
                  <div className="schedule-preset-missing">
                    <span>引用的启动方案「{selected.preset_id}」已不存在，这条计划只会按自带的参数运行。</span>
                    <button type="button" className="btn" onClick={beginEdit}>重新选择方案</button>
                  </div>
                ) : null}
              </section>
            ) : null}
            {!editMode && selected ? (
              <section className="workflow-section">
                <div className="workflow-section-title"><strong>执行情况</strong></div>
                <div className="workflow-run-context is-schedule">
                  {/* 停用的计划不会再跑，next_run_at 也不再推进：这一格说状态，不说一个不会发生的时刻。 */}
                  <div>
                    <span>下次执行</span>
                    <strong>{selected.enabled ? formatCompactStamp(selected.next_run_at, nowMs) : '已停用'}</strong>
                    {/* 出路跟着状态写在这里，而不是再占一行说明：启停控件只在左侧列表行尾，
                        详情不再挂第二枚按钮，也没有第二处说「已停用」。 */}
                    <small>{selected.enabled ? formatCountdown(selected.next_run_at, nowMs) : '打开左侧的开关后恢复自动运行'}</small>
                  </div>
                  <div><span>上次执行</span><strong>{formatStamp(selected.last_run_at)}</strong></div>
                  <div><span>上次结果</span><strong>{lastResultLabel(selected)}</strong></div>
                </div>
                {selected.kind === 'skill' && selected.last_run_id ? (
                  <div className="schedule-actions">
                    <button type="button" className="btn" onClick={() => void onRevealSkillRun(selected.last_run_id)}>
                      <FolderOpen size={14} />打开上次结果
                    </button>
                  </div>
                ) : null}
                {selected.last_error ? <div className="workflow-inline-error">{selected.last_error}</div> : null}
              </section>
            ) : null}
            {!editMode && selected?.kind === 'skill' ? (
              <section className="workflow-section">
                <div className="workflow-section-title">
                  <strong>最近的技能运行</strong>
                </div>
                <div className="workflow-run-artifacts">
                  {skillRuns.map(run => (
                    <div key={run.run_id} className={run.status === 'running' ? 'is-live' : ''}>
                      <BookOpen size={14} />
                      <span>
                        <strong className={run.status === 'running' ? 'run-live-label' : ''}>{run.skill_name} · {run.status === 'succeeded' ? '已完成' : run.status === 'running' ? '运行中' : '失败'}</strong>
                        <small>{run.task} · {formatStamp(run.started_at)}{run.duration_seconds ? ` · ${run.duration_seconds}s` : ''}</small>
                        {run.error ? <small>{run.error}</small> : null}
                      </span>
                      <button type="button" className="btn btn-icon" title="打开结果" aria-label={`打开 ${run.run_id} 的结果`} onClick={() => void onRevealSkillRun(run.run_id)}><FolderOpen size={14} /></button>
                    </div>
                  ))}
                  {skillRuns.length === 0 ? <div className="workflow-start-empty">还没有技能运行记录。</div> : null}
                </div>
              </section>
            ) : null}
          </div>
          </div>
          {/* 底部条只在编辑/新建时出现：它是表单页脚（保存 / 取消），
              不是对象级动作区——读态的动作已经在头部，这里不留空条。 */}
          {editMode ? (
            <div className="schedule-actions schedule-actions-sticky">
              <button
                type="button"
                className="btn btn-primary"
                disabled={Boolean(busy) || preflightBusy}
                title="保存前会先检查依赖和凭据，避免到点才发现跑不起来。"
                onClick={() => void save(false)}
              >
                {busy === 'save' ? '保存中' : '保存定时计划'}
              </button>
              {saveSkippingPreflight ? (
                <button
                  type="button"
                  className="btn"
                  title="当前运行条件未就绪；计划仍会保存，执行时再处理依赖。"
                  disabled={Boolean(busy) || preflightBusy}
                  onClick={() => void save(true)}
                >
                  {busy === 'save' ? '保存中' : '仍然保存'}
                </button>
              ) : null}
              <button type="button" className="btn" disabled={Boolean(busy) || preflightBusy} onClick={cancelEdit}>取消</button>
            </div>
          ) : null}
        </section>
      </div>
      {/* 站内确认弹窗：和工作流详情「移除工作流」同一套 .modal，标题说清动作，
          说明写清后果。原生 confirm 不在这里出现。 */}
      {pendingConfirm ? (
        <div className="modal-backdrop" role="presentation" onClick={event => { if (event.currentTarget === event.target) setPendingConfirm(null); }}>
          <section className="modal workflow-confirm-modal" role="dialog" aria-modal="true" aria-labelledby="schedule-confirm-title">
            <div className="modal-header">
              <div>
                <h3 id="schedule-confirm-title">{pendingConfirm.title}</h3>
                <p>{pendingConfirm.description}</p>
              </div>
              <button type="button" className="btn btn-icon" title="关闭" aria-label="关闭" onClick={() => setPendingConfirm(null)}><X size={16} /></button>
            </div>
            <div className="modal-body">
              <div className="modal-actions">
                <button type="button" className="btn" onClick={() => setPendingConfirm(null)}>取消</button>
                <button
                  type="button"
                  className="btn btn-danger"
                  onClick={() => {
                    const action = pendingConfirm;
                    setPendingConfirm(null);
                    action.run();
                  }}
                >
                  {pendingConfirm.confirmText}
                </button>
              </div>
            </div>
          </section>
        </div>
      ) : null}
    </div>
  );
}
