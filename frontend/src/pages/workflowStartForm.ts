// 启动表单的纯转换逻辑：视图字段 → 表单值 → 运行输入。
// 单独成模块是为了能被直接验证：这里的转换一旦错位，
// 用户会在「启动后运行失败」里才发现（例如列表被写成一行 "cv,llm,ar-vr"）。
import type { WorkflowViewField } from '../services/agentApi';

export type WorkflowStartField = {
  id: string;
  label: string;
  type: string;
  required: boolean;
  defaultValue: unknown;
  /** 允许取值（提交给能力的机器值）。 */
  options: string[];
  /**
   * 选项的「值 → 显示文本」。包只写了字符串时两者相同；
   * 写了 `{ value, label }` 时下拉显示 label、提交 value。
   */
  optionEntries: Array<{ value: string; label: string; topics: string[]; source?: string; description?: string }>;
  aliases: string[];
  optionSourceWarning: string;
  optionSourceCached: boolean;
  placeholder: string;
  target: string;
  picker: string;
  hint: string;
  budget: { maxQueries?: number; intervalMs?: number };
  span: string;
};

type OptionSource = string | { value?: unknown; label?: unknown; topics?: unknown; source?: unknown; description?: unknown };

function stringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.map(item => String(item).trim()).filter(Boolean) : [];
}

function toOptionEntries(options: OptionSource[] | undefined): Array<{ value: string; label: string; topics: string[]; source?: string; description?: string }> {
  return (options || []).map(option => {
    if (typeof option === 'string') return { value: option, label: option, topics: [] };
    const value = String(option.value ?? '');
    const entry: { value: string; label: string; topics: string[]; source?: string; description?: string } = {
      value,
      label: String(option.label ?? '') || value,
      topics: stringArray(option.topics),
    };
    const source = String(option.source ?? '').trim();
    const description = String(option.description ?? '').trim();
    if (source) entry.source = source;
    if (description) entry.description = description;
    return entry;
  });
}

// 字段级校验错误：抛到提交层时带着字段 id，界面才能把提示放回出错的那一行，
// 而不是笼统地丢在弹窗底部让人自己翻。
export class WorkflowStartFieldError extends Error {
  readonly fieldId: string;

  constructor(field: WorkflowStartField, message?: string) {
    super(message || `${field.label}不能为空`);
    this.fieldId = field.id;
  }
}

export function normalizeStartField(field: WorkflowViewField): WorkflowStartField {
  if (typeof field === 'string') {
    const type = field === 'credential_handles'
      ? 'credential'
      : field === 'acceptance_criteria' || field === 'constraints' || field === 'scripts' || field === 'evidence_paths'
        ? 'list'
        : field === 'passed' || field === 'rollback_requested'
          ? 'boolean'
          : field === 'package_manager' || field === 'install_mode' || field === 'environment'
            ? 'select'
            : 'text';
    const options = field === 'package_manager'
      ? ['npm', 'pnpm', 'yarn']
      : field === 'install_mode'
        ? ['install', 'ci']
        : field === 'environment'
          ? ['development', 'staging', 'production']
          : [];
    return {
      id: field,
      label: field,
      type,
      required: field === 'requirement' || field === 'acceptance_criteria' || field === 'workspace_root' || field === 'project_root' || field === 'app_id',
      defaultValue: field === 'passed' ? true : field === 'rollback_requested' ? false : field === 'package_manager' ? 'npm' : field === 'install_mode' ? 'install' : field === 'environment' ? 'development' : '',
      options,
      optionEntries: toOptionEntries(options),
      aliases: [],
      optionSourceWarning: '',
      optionSourceCached: false,
      placeholder: '',
      target: field === 'credential_handles' ? 'private_key_path' : '',
      picker: '',
      hint: '',
      budget: {},
      span: '',
    };
  }
  const optionEntries = toOptionEntries(field.options);
  return {
    id: field.id,
    label: field.label || field.id,
    type: field.type || 'text',
    required: Boolean(field.required),
    defaultValue: field.default,
    options: optionEntries.map(option => option.value),
    optionEntries,
    aliases: Array.isArray(field.aliases) ? field.aliases.map(String).filter(Boolean) : [],
    optionSourceWarning: field.option_source_warning || '',
    optionSourceCached: Boolean(field.option_source_cached),
    placeholder: field.placeholder || '',
    target: field.target || '',
    picker: field.picker || '',
    hint: field.hint || '',
    budget: {
      maxQueries: field.budget?.max_queries,
      intervalMs: field.budget?.interval_ms,
    },
    span: field.span || '',
  };
}

// 字段声明了 options 就代表「只能取这些值」。列表字段逐行校验，
// 这样「把 cv、llm、ar-vr 写成一行」这类错误在点启动时就报出来。
export function invalidOptionValue(field: WorkflowStartField, value: unknown) {
  if (field.type === 'tag-list') {
    return stringArray(value).find(item => !CUSTOM_TOPIC_PATTERN.test(item)) || '';
  }
  if (field.type === 'topic-groups') {
    return normalizeTopicGroupsValue(value)
      .flatMap(group => group.topics)
      .find(topic => !CUSTOM_TOPIC_PATTERN.test(topic)) || '';
  }
  if (!field.options.length) return '';
  const allowed = new Set(field.options);
  if (field.type === 'list') {
    const items = String(value ?? '').split(/\r?\n/).map(item => item.trim()).filter(Boolean);
    return items.find(item => !allowed.has(item)) || '';
  }
  if (field.type === 'theme-list') {
    return stringArray(value).find(item => !allowed.has(item)) || '';
  }

  if (field.type === 'select') {
    const current = String(value ?? '').trim();
    return current && !allowed.has(current) ? current : '';
  }
  return '';
}

// 整行字段：显式声明 span=full，或本身需要一个可读的多行/结构化输入区。
export function usesFullRow(field: WorkflowStartField) {
  return field.span === 'full' || field.type === 'textarea' || field.type === 'list' || field.type === 'theme-list' || field.type === 'tag-list' || field.type === 'topic-groups' || field.type === 'json';
}

export function initialFieldValue(field: WorkflowStartField): unknown {
  if (field.defaultValue !== undefined) {
    if (field.type === 'list' && Array.isArray(field.defaultValue)) return field.defaultValue.join('\n');
    return field.defaultValue;
  }
  if (field.type === 'boolean') return false;
  if (field.type === 'list') return '';
  if (field.type === 'theme-list' || field.type === 'tag-list') return [];
  if (field.type === 'topic-groups') return Array.isArray(field.defaultValue) ? normalizeTopicGroupsValue(field.defaultValue) : [];
  if (field.type === 'credential') return field.target === 'private_key_path' ? 'wechat-upload-private-key' : '';
  if (field.type === 'json') return '{}';
  return '';
}

export function fieldsToInput(fields: WorkflowStartField[], values: Record<string, unknown>): Record<string, unknown> {
  const input: Record<string, unknown> = {};
  for (const field of fields) {
    const value = values[field.id];
    if (field.type === 'credential') {
      const handle = String(value || '').trim();
      if (handle) input[field.id] = { [field.target || 'value']: handle };
      continue;
    }
    if (field.type === 'list') {
      input[field.id] = String(value || '').split(/\r?\n/).map(item => item.trim()).filter(Boolean);
      continue;
    }
    if (field.type === 'theme-list' || field.type === 'tag-list') {
      input[field.id] = stringArray(value);
      continue;
    }
    if (field.type === 'topic-groups') {
      input[field.id] = normalizeTopicGroupsValue(value);
      continue;
    }
    if (field.type === 'number') {
      input[field.id] = value === '' ? 0 : Number(value);
      continue;
    }
    if (field.type === 'json') {
      input[field.id] = JSON.parse(String(value || '{}'));
      continue;
    }
    input[field.id] = value ?? '';
  }
  return input;
}

export function jsonToFormValues(fields: WorkflowStartField[], input: Record<string, unknown>): Record<string, unknown> {
  const values: Record<string, unknown> = {};
  for (const field of fields) {
    const value = inputValueForField(input, field);
    if (field.type === 'credential') {
      values[field.id] = value && typeof value === 'object' && !Array.isArray(value)
        ? String((value as Record<string, unknown>)[field.target || 'value'] || '')
        : '';
    } else if (field.type === 'list') {
      values[field.id] = Array.isArray(value) ? value.join('\n') : String(value || '');
    } else if (field.type === 'theme-list' || field.type === 'tag-list') {
      values[field.id] = stringArray(value);
    } else if (field.type === 'topic-groups') {
      values[field.id] = normalizeTopicGroupsValue(value);
    } else if (field.type === 'json') {
      values[field.id] = JSON.stringify(value ?? {}, null, 2);
    } else {
      values[field.id] = value ?? initialFieldValue(field);
    }
  }
  return values;
}

/**
 * 摘要里的「已填 N/M」：布尔 false、空列表、空对象都算没填，
 * 否则 passed=false、rollback_requested=false 这类默认值会把计数虚高一截。
 */
function isFilledValue(value: unknown) {
  if (Array.isArray(value)) return value.length > 0;
  if (typeof value === 'boolean') return value;
  if (value && typeof value === 'object') return Object.keys(value as Record<string, unknown>).length > 0;
  return String(value ?? '').trim() !== '';
}

export function filledFieldCount(fields: WorkflowStartField[], values: Record<string, unknown>) {
  return fields.filter(field => isFilledValue(values[field.id])).length;
}

// 表单初始值 = 包声明的默认值 + 预设覆盖。
// 预设里出现的字段才覆盖；列表必须经过与 JSON→表单同一条转换，
// 否则数组会被 String() 拍成一行 "cv,llm,ar-vr"，提交后就是非法取值。
function hasFieldValue(input: Record<string, unknown>, field: WorkflowStartField) {
  return field.id in input || field.aliases.some(alias => alias in input);
}

function inputValueForField(input: Record<string, unknown>, field: WorkflowStartField): unknown {
  if (field.id in input) return input[field.id];
  for (const alias of field.aliases) {
    if (alias in input) return input[alias];
  }
  return undefined;
}

export function initialFormValues(
  fields: WorkflowStartField[],
  prefill?: Record<string, unknown>,
): Record<string, unknown> {
  const values: Record<string, unknown> = {};
  for (const field of fields) values[field.id] = initialFieldValue(field);
  if (!prefill) return values;
  const converted = jsonToFormValues(fields, prefill);
  for (const field of fields) {
    if (!hasFieldValue(prefill, field)) continue;
    const source = inputValueForField(prefill, field);
    // JSON 字段允许预设直接存字符串，这种情况原样保留，不要再 stringify 一次。
    values[field.id] = field.type === 'json' && typeof source === 'string'
      ? String(source)
      : converted[field.id];
  }
  return values;
}

export const CUSTOM_TOPIC_PATTERN = /^[a-z0-9][a-z0-9-]{0,49}$/;

export type TopicGroupFormValue = {
  id: string;
  name: string;
  topics: string[];
};

function normalizeTopicGroupsValue(value: unknown): TopicGroupFormValue[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((raw, index) => {
    if (!raw || typeof raw !== 'object') return [];
    const object = raw as Record<string, unknown>;
    const topics = stringArray(object.topics).filter(topic => CUSTOM_TOPIC_PATTERN.test(topic));
    return [{
      id: String(object.id ?? '').trim() || `group-${index + 1}`,
      name: String(object.name ?? '').trim() || `采集组 ${index + 1}`,
      topics: Array.from(new Set(topics)),
    }];
  });
}

export function normalizeTopicGroupsSafe(value: unknown): TopicGroupFormValue[] {
  return normalizeTopicGroupsValue(value);
}

export function newTopicGroup(index: number): TopicGroupFormValue {
  return { id: `group-${Date.now()}-${index + 1}`, name: `采集组 ${index + 1}`, topics: [] };
}

export type ThemeSelectionPlan = {
  themeField: WorkflowStartField;
  selectedThemeIds: string[];
  selectedThemeLabels: string[];
  customTopics: string[];
  resolvedTopics: string[];
  queryCount: number;
  estimatedSeconds: number;
  maxQueries: number;
};

/**
 * 科技雷达的主题预算只依赖视图元数据：每个预设声明自己的 topic，
 * 每个 topic 会展开 active/rising 两次查询。这样保存前就能拦住明显超限的组合。
 */
export function themeSelectionPlan(
  fields: WorkflowStartField[],
  values: Record<string, unknown>,
): ThemeSelectionPlan | null {
  const themeField = fields.find(field => field.type === 'theme-list');
  if (!themeField) return null;
  const selectedThemeIds = stringArray(values[themeField.id]);
  const selected = themeField.optionEntries.filter(option => selectedThemeIds.includes(option.value));
  const customTopics = fields
    .filter(field => field.type === 'tag-list')
    .flatMap(field => stringArray(values[field.id]));
  const resolved = new Set<string>();
  for (const option of selected) {
    for (const topic of option.topics) resolved.add(topic);
  }
  for (const topic of customTopics) resolved.add(topic);
  const queryCount = resolved.size * 2;
  const maxQueries = themeField.budget.maxQueries ?? 24;
  const intervalMs = themeField.budget.intervalMs ?? 7000;
  return {
    themeField,
    selectedThemeIds,
    selectedThemeLabels: selected.map(option => option.label),
    customTopics: Array.from(new Set(customTopics)),
    resolvedTopics: Array.from(resolved),
    queryCount,
    estimatedSeconds: queryCount > 0 ? Math.ceil((queryCount - 1) * intervalMs / 1000) : 0,
    maxQueries,
  };
}

export function themeBudgetError(fields: WorkflowStartField[], values: Record<string, unknown>): string {
  const groupField = fields.find(field => field.type === 'topic-groups');
  if (groupField) {
    const groups = normalizeTopicGroupsValue(values[groupField.id]);
    const maxGroups = 4;
    if (!groups.length) return '请至少添加一个采集组';
    if (groups.length > maxGroups) return `一个计划最多支持 ${maxGroups} 个采集组`;
    if (groups.some(group => !group.name.trim())) return '每个采集组都需要名称';
    if (groups.some(group => !group.topics.length)) return '每个采集组至少需要一个标签';
    const resolved = new Set(groups.flatMap(group => group.topics));
    const queryCount = resolved.size * 2;
    const maxQueries = groupField.budget.maxQueries ?? 24;
    if (queryCount > maxQueries) {
      return `${groupField.label}需要 ${queryCount} 次查询，超过上限 ${maxQueries}；请减少组内标签`;
    }
    return '';
  }
  const plan = themeSelectionPlan(fields, values);
  if (!plan) return '';
  if (!plan.selectedThemeIds.length && !plan.customTopics.length) return '请至少选择一个分类或填写一个额外 topic';
  if (plan.queryCount > plan.maxQueries) {
    return `${plan.themeField.label}需要 ${plan.queryCount} 次查询，超过上限 ${plan.maxQueries}；请减少分类或额外 topic`;
  }
  return '';
}

export type TopicGroupsSelectionPlan = {
  field: WorkflowStartField;
  groups: TopicGroupFormValue[];
  resolvedTopics: string[];
  queryCount: number;
  estimatedSeconds: number;
  maxQueries: number;
  maxGroups: number;
};

export function topicGroupsSelectionPlan(
  fields: WorkflowStartField[],
  values: Record<string, unknown>,
): TopicGroupsSelectionPlan | null {
  const field = fields.find(item => item.type === 'topic-groups');
  if (!field) return null;
  const groups = normalizeTopicGroupsValue(values[field.id]);
  const resolvedTopics = Array.from(new Set(groups.flatMap(group => group.topics)));
  const queryCount = resolvedTopics.length * 2;
  const maxQueries = field.budget.maxQueries ?? 24;
  const intervalMs = field.budget.intervalMs ?? 7000;
  return {
    field,
    groups,
    resolvedTopics,
    queryCount,
    estimatedSeconds: queryCount > 0 ? Math.ceil((queryCount - 1) * intervalMs / 1000) : 0,
    maxQueries,
    maxGroups: 4,
  };
}
