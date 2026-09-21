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
  options: string[];
  placeholder: string;
  target: string;
  picker: string;
  hint: string;
  span: string;
};

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
      placeholder: '',
      target: field === 'credential_handles' ? 'private_key_path' : '',
      picker: '',
      hint: '',
      span: '',
    };
  }
  return {
    id: field.id,
    label: field.label || field.id,
    type: field.type || 'text',
    required: Boolean(field.required),
    defaultValue: field.default,
    options: field.options || [],
    placeholder: field.placeholder || '',
    target: field.target || '',
    picker: field.picker || '',
    hint: field.hint || '',
    span: field.span || '',
  };
}

// 字段声明了 options 就代表「只能取这些值」。列表字段逐行校验，
// 这样「把 cv、llm、ar-vr 写成一行」这类错误在点启动时就报出来。
export function invalidOptionValue(field: WorkflowStartField, value: unknown) {
  if (!field.options.length) return '';
  const allowed = new Set(field.options);
  if (field.type === 'list') {
    const items = String(value ?? '').split(/\r?\n/).map(item => item.trim()).filter(Boolean);
    return items.find(item => !allowed.has(item)) || '';
  }
  if (field.type === 'select') {
    const current = String(value ?? '').trim();
    return current && !allowed.has(current) ? current : '';
  }
  return '';
}

// 整行字段：显式声明 span=full，或本身需要一个可读的多行/结构化输入区。
export function usesFullRow(field: WorkflowStartField) {
  return field.span === 'full' || field.type === 'textarea' || field.type === 'list' || field.type === 'json';
}

export function initialFieldValue(field: WorkflowStartField): unknown {
  if (field.defaultValue !== undefined) {
    if (field.type === 'list' && Array.isArray(field.defaultValue)) return field.defaultValue.join('\n');
    return field.defaultValue;
  }
  if (field.type === 'boolean') return false;
  if (field.type === 'list') return '';
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
    const value = input[field.id];
    if (field.type === 'credential') {
      values[field.id] = value && typeof value === 'object' && !Array.isArray(value)
        ? String((value as Record<string, unknown>)[field.target || 'value'] || '')
        : '';
    } else if (field.type === 'list') {
      values[field.id] = Array.isArray(value) ? value.join('\n') : String(value || '');
    } else if (field.type === 'json') {
      values[field.id] = JSON.stringify(value ?? {}, null, 2);
    } else {
      values[field.id] = value ?? initialFieldValue(field);
    }
  }
  return values;
}

// 表单初始值 = 包声明的默认值 + 预设覆盖。
// 预设里出现的字段才覆盖；列表必须经过与 JSON→表单同一条转换，
// 否则数组会被 String() 拍成一行 "cv,llm,ar-vr"，提交后就是非法取值。
export function initialFormValues(
  fields: WorkflowStartField[],
  prefill?: Record<string, unknown>,
): Record<string, unknown> {
  const values: Record<string, unknown> = {};
  for (const field of fields) values[field.id] = initialFieldValue(field);
  if (!prefill) return values;
  const converted = jsonToFormValues(fields, prefill);
  for (const field of fields) {
    if (!(field.id in prefill)) continue;
    // JSON 字段允许预设直接存字符串，这种情况原样保留，不要再 stringify 一次。
    values[field.id] = field.type === 'json' && typeof prefill[field.id] === 'string'
      ? String(prefill[field.id])
      : converted[field.id];
  }
  return values;
}
