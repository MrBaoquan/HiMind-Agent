/**
 * 把 interaction_request.schema 转成可渲染的表单描述。
 *
 * 反馈原本只有一个 8000 字 textarea：用户用自然语言描述「下一轮改什么」，
 * 模型再把它解析回可执行指令，两次转述都在丢信息。schema 早已存在，
 * 只是一直以 JSON 原样展示——这里把它变成真正的表单。
 *
 * 只覆盖有真实场景的五类字段。其余类型（数组嵌套、$ref、oneOf）回落到
 * 文本输入，并明确告知用户「这里仍是自由文本」，不假装支持。
 */
import type { WorkflowInteractionRequest } from '../services/agentApi';

export type FormFieldKind = 'text' | 'textarea' | 'number' | 'boolean' | 'select';

export type InteractionFormField = {
  name: string;
  label: string;
  kind: FormFieldKind;
  required: boolean;
  defaultValue: unknown;
  options: Array<{ value: string; label: string }>;
  hint: string;
  /** 同组字段的说明（schema 顶层 description）。 */
  groupHint: string;
};

export type InteractionForm = {
  fields: InteractionFormField[];
  /** 无法结构化渲染时的原因；非空表示应回落到文本框。 */
  fallbackReason: string;
};

const JSON_TYPE_TO_KIND: Record<string, FormFieldKind> = {
  string: 'text',
  number: 'number',
  integer: 'number',
  boolean: 'boolean',
};

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

/** 从一个 properties 条目推导字段类型；推不出来就返回 null。 */
function deriveKind(property: Record<string, unknown>): FormFieldKind | null {
  const rawType = property.type;
  if (typeof rawType === 'string') {
    if (rawType === 'array') {
      // 数组只有「枚举数组」有意义（勾选列表）；自由数组交给文本。
      const items = property.items;
      if (isPlainObject(items) && Array.isArray(items.enum)) return 'select';
      return null;
    }
    if (rawType === 'string') {
      // 有枚举 → 下拉；声明了 format=textarea 或长文本 → 多行。
      if (Array.isArray(property.enum)) return 'select';
      if (property.format === 'textarea') return 'textarea';
      return 'text';
    }
    return JSON_TYPE_TO_KIND[rawType] ?? null;
  }
  // 没写 type 但有 enum：按下拉处理。
  if (Array.isArray(property.enum)) return 'select';
  return null;
}

function optionsFor(property: Record<string, unknown>): Array<{ value: string; label: string }> {
  const source = Array.isArray(property.enum)
    ? property.enum
    : isPlainObject(property.items) && Array.isArray(property.items.enum)
      ? property.items.enum
      : [];
  return source.map(item => {
    if (isPlainObject(item)) {
      const value = String(item.value ?? '');
      return { value, label: String(item.label ?? value) };
    }
    return { value: String(item), label: String(item) };
  });
}

/**
 * 解析 interaction_request 的 schema。
 *
 * 返回 `fallbackReason` 非空的表单表示「不具备结构化条件」，调用方应保留原文本框。
 * 这条路径必须始终可用：schema 是可选字段，缺了不能让用户没法反馈。
 */
export function buildInteractionForm(request: WorkflowInteractionRequest | null): InteractionForm {
  const empty: InteractionForm = { fields: [], fallbackReason: '' };
  if (!request) return { ...empty, fallbackReason: '没有待处理的交互请求' };
  const schema = request.schema;
  if (!schema || !isPlainObject(schema)) {
    return { ...empty, fallbackReason: '该步骤没有声明提交格式' };
  }
  const properties = schema.properties;
  if (!isPlainObject(properties)) {
    return { ...empty, fallbackReason: '提交格式未声明字段' };
  }
  // 只支持顶层对象 + 简单字段。出现 $ref/allOf/oneOf 时整体回落，
  // 而不是「部分渲染 + 静默丢弃几项」——那会让用户以为反馈完整提交了。
  for (const key of ['$ref', 'allOf', 'anyOf', 'oneOf']) {
    if (schema[key] !== undefined) {
      return { ...empty, fallbackReason: '提交格式使用了组合语义，暂不支持表单渲染' };
    }
  }

  const required = new Set(
    Array.isArray(schema.required) ? schema.required.filter((item): item is string => typeof item === 'string') : [],
  );
  const groupHint = typeof schema.description === 'string' ? schema.description.trim() : '';
  const fields: InteractionFormField[] = [];
  for (const [name, property] of Object.entries(properties)) {
    if (!isPlainObject(property)) {
      return { ...empty, fallbackReason: `字段 ${name} 的定义不是对象` };
    }
    const kind = deriveKind(property);
    if (!kind) {
      // 有一个字段无法结构化 → 整体回落。混着渲染会误导。
      return { ...empty, fallbackReason: `字段 ${name} 的类型暂不支持表单渲染` };
    }
    fields.push({
      name,
      label: typeof property.title === 'string' && property.title.trim() ? property.title.trim() : name,
      kind,
      required: required.has(name),
      defaultValue: property.default,
      options: optionsFor(property),
      hint: typeof property.description === 'string' ? property.description.trim() : '',
      groupHint,
    });
  }
  if (fields.length === 0) {
    return { ...empty, fallbackReason: '提交格式未声明字段' };
  }
  return { fields, fallbackReason: '' };
}

/** 表单初值：有 default 用 default，否则按类型给空值。 */
export function initialFormState(fields: InteractionFormField[]): Record<string, unknown> {
  const state: Record<string, unknown> = {};
  for (const field of fields) {
    if (field.defaultValue !== undefined) {
      state[field.name] = field.defaultValue;
      continue;
    }
    if (field.kind === 'boolean') state[field.name] = false;
    else if (field.kind === 'number') state[field.name] = '';
    else state[field.name] = '';
  }
  return state;
}

/**
 * 校验并序列化成提交用的 feedback 文本。
 *
 * 提交格式仍是字符串（resume 接口收文本），但内容是结构化的 JSON——
 * 这样既不改后端契约，模型也能直接解析，不用猜自然语言。
 */
export function formToFeedback(
  fields: InteractionFormField[],
  state: Record<string, unknown>,
): { feedback: string; error: string } {
  const payload: Record<string, unknown> = {};
  for (const field of fields) {
    const value = state[field.name];
    const blank = value === '' || value === null || value === undefined;
    if (field.required && blank) {
      return { feedback: '', error: `「${field.label}」是必填项` };
    }
    if (blank) continue;
    if (field.kind === 'number') {
      const numeric = Number(value);
      if (!Number.isFinite(numeric)) {
        return { feedback: '', error: `「${field.label}」必须是数字` };
      }
      payload[field.name] = numeric;
      continue;
    }
    payload[field.name] = value;
  }
  return { feedback: JSON.stringify(payload), error: '' };
}
