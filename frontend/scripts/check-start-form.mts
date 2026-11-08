// 启动表单转换的回归自检（零依赖：node --experimental-strip-types）。
// 覆盖一次真实事故：预设里的列表被 String() 拍成一行 "cv,llm,ar-vr"，
// 运行期才在能力入参校验里报错，而用户只看到「运行失败」。
import { strict as assert } from 'node:assert';
import {
  fieldsToExecutionOverrides,
  fieldsToInput,
  initialFormValues,
  invalidOptionValue,
  normalizeStartField,
  themeBudgetError,
  themeSelectionPlan,
} from '../src/pages/workflowStartForm.ts';

const fields = [
  normalizeStartField({
    id: 'domains',
    label: '关注领域',
    type: 'list',
    default: ['cv', 'llm', 'ar-vr'],
    options: ['cv', 'llm', 'ar-vr', 'general'],
  }),
  normalizeStartField({ id: 'top_n', label: '每段条目数', type: 'number', default: 10 }),
  normalizeStartField({ id: 'report_root', label: '报告归档目录', type: 'text' }),
];

// 预设（数组）→ 表单文本 → 运行输入，必须还是三个独立取值。
const fromPreset = initialFormValues(fields, {
  domains: ['cv', 'llm', 'ar-vr'],
  top_n: 10,
  report_root: '',
});
assert.equal(fromPreset.domains, 'cv\nllm\nar-vr');
assert.deepEqual(fieldsToInput(fields, fromPreset).domains, ['cv', 'llm', 'ar-vr']);

// 没有预设时走包声明的默认值，同样是三个取值。
assert.deepEqual(fieldsToInput(fields, initialFormValues(fields)).domains, ['cv', 'llm', 'ar-vr']);

// 手填非法取值要在本地被识别，而不是等一次注定失败的执行。
const typed = initialFormValues(fields);
typed.domains = 'cv,llm,ar-vr';
assert.equal(invalidOptionValue(fields[0], typed.domains), 'cv,llm,ar-vr');
assert.equal(invalidOptionValue(fields[0], 'cv\ngeneral'), '');

// 带显示名称的下拉：下拉显示中文，提交与校验都走机器值。
// 包只写字符串时两者相同，不能因为补了 label 就把「展馆」提交成「三峡」。
const venue = normalizeStartField({
  id: 'venue',
  label: '展馆',
  type: 'select',
  required: true,
  options: [
    { value: 'szkjg', label: '随州科技馆' },
    { value: 'sanxia', label: '三峡' },
    'hdcybwg',
  ],
});
assert.deepEqual(venue.optionEntries, [
  { value: 'szkjg', label: '随州科技馆', topics: [] },
  { value: 'sanxia', label: '三峡', topics: [] },
  { value: 'hdcybwg', label: 'hdcybwg', topics: [] },
]);
assert.deepEqual(venue.options, ['szkjg', 'sanxia', 'hdcybwg']);
assert.equal(invalidOptionValue(venue, 'sanxia'), '');
assert.equal(invalidOptionValue(venue, '三峡'), '三峡');
assert.equal(fieldsToInput([venue], { venue: 'sanxia' }).venue, 'sanxia');

// theme-list / tag-list：预设多选与自定义 topic 都保持数组，不压成逗号文本。
const themeFields = [
  normalizeStartField({
    id: 'category_ids',
    label: '关注分类',
    type: 'theme-list',
    default: ['ai-video'],
    aliases: ['domains'],
    options: [
      { value: 'ai-video', label: 'AI 视频生成', topics: ['text-to-video', 'video-generation'], source: 'system' },
      { value: 'code-animation', label: '代码动画视频', topics: ['remotion', 'programmatic-video'], source: 'system' },
    ],
    option_source: { kind: 'plugin_capability', capability_id: 'tech-radar.themes' },
    option_source_warning: '已使用最近缓存',
    budget: { max_queries: 4, interval_ms: 1000 },
  }),
  normalizeStartField({ id: 'extra_topics', label: '额外 topic', type: 'tag-list', aliases: ['topics'] }),
];
assert.equal(themeFields[0].optionSourceWarning, '已使用最近缓存');
assert.equal(themeFields[0].optionEntries[0].source, 'system');
// 旧计划的 domains/topics 自动读到新字段，提交时只写新字段。
const themeValues = initialFormValues(themeFields, { domains: ['ai-video'], topics: ['remotion'] });
assert.deepEqual(themeValues.category_ids, ['ai-video']);
assert.deepEqual(themeValues.extra_topics, ['remotion']);
assert.deepEqual(fieldsToInput(themeFields, themeValues), { category_ids: ['ai-video'], extra_topics: ['remotion'] });
assert.equal(invalidOptionValue(themeFields[1], ['AI Agent']), 'AI Agent');
const plan = themeSelectionPlan(themeFields, { category_ids: ['ai-video', 'code-animation'], extra_topics: [] });
assert.equal(plan?.queryCount, 8);
assert.equal(plan?.resolvedTopics.includes('remotion'), true);
assert.match(themeBudgetError(themeFields, { category_ids: ['ai-video', 'code-animation'], extra_topics: [] }) || '', /超过上限/);
// ── override 字段：不进步骤输入，落成 execution_overrides ──────────────────
const overrideFields = [
  normalizeStartField({
    id: 'max_rounds',
    label: '最多打磨轮次',
    type: 'number',
    default: 3,
    override: { stepId: 'POLISH-LOOP', key: 'max_iterations' },
  }),
  normalizeStartField({
    id: 'workspace_root',
    label: '工作区',
    type: 'text',
    required: true,
  }),
];

{
  const values = { max_rounds: 5, workspace_root: '/repo' };
  // override 字段不得出现在步骤输入里，否则会被当成参数传给模型。
  const input = fieldsToInput(overrideFields, values);
  assert.equal(input.max_rounds, undefined, 'override 字段不能进步骤输入');
  assert.equal(input.workspace_root, '/repo', '普通字段照常进输入');

  const overrides = fieldsToExecutionOverrides(overrideFields, values);
  assert.deepEqual(
    overrides,
    { 'POLISH-LOOP': { max_iterations: 5 } },
    '轮数应落成对应步骤的覆盖',
  );
}

// 空值 / 非数字 / 0 都不产生覆盖：宁可让包用自己的默认值，也不要写一个非法轮数。
{
  for (const bad of ['', 'abc', 0, -1, null, undefined]) {
    const overrides = fieldsToExecutionOverrides(overrideFields, {
      max_rounds: bad,
      workspace_root: '/repo',
    });
    assert.deepEqual(overrides, {}, `值 ${String(bad)} 不应产生覆盖`);
  }
}

// 小数向下取整：轮数是整数概念。
{
  const overrides = fieldsToExecutionOverrides(overrideFields, { max_rounds: 4.7 });
  assert.equal((overrides['POLISH-LOOP'] as { max_iterations: number }).max_iterations, 4);
}

console.log('start form conversion checks passed');

// topic-groups：计划内采集组保存为对象数组，跨组同名 topic 只计一次预算。
const groupFields = [
  {
    id: 'interest_groups',
    label: '采集内容',
    type: 'topic-groups',
    required: true,
    defaultValue: [],
    options: ['text-to-video', 'remotion', 'unity3d'],
    optionEntries: [
      { value: 'text-to-video', label: '文生视频', topics: [], source: 'system' },
      { value: 'remotion', label: 'Remotion', topics: [], source: 'system' },
      { value: 'unity3d', label: 'Unity3D', topics: [], source: 'system' },
    ],
    aliases: [],
    optionSourceWarning: '',
    optionSourceCached: false,
    placeholder: '',
    target: '',
    picker: '',
    hint: '',
    budget: { maxQueries: 4, intervalMs: 1000 },
    span: 'full',
  },
];
const groupValues = {
  interest_groups: [
    { id: 'media', name: 'AI 视频', topics: ['text-to-video', 'remotion'] },
    { id: 'dev', name: '实时 3D', topics: ['remotion', 'unity3d'] },
  ],
};
assert.deepEqual(fieldsToInput(groupFields as any, groupValues).interest_groups, groupValues.interest_groups);
assert.match(themeBudgetError(groupFields as any, groupValues) || '', /超过上限/);
assert.equal(themeBudgetError(groupFields as any, {
  interest_groups: [{ id: 'media', name: 'AI 视频', topics: ['text-to-video', 'remotion'] }],
}) || '', '');