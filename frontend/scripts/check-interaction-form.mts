// 结构化反馈表单的自检（零依赖：node --experimental-strip-types）。
//
// 关键不变量：
//   1. schema 能解析 → 渲染表单；不能解析 → 明确回落文本框（不许半渲染）
//   2. 必填校验在前端拦住，错误文案指出具体字段
//   3. 提交内容是 JSON 文本，后端契约不变
import { strict as assert } from 'node:assert';
import {
  buildInteractionForm,
  formToFeedback,
  initialFormState,
} from '../src/pages/interactionForm.ts';

function request(schema: unknown) {
  return {
    schema_version: 'interaction_request.v1',
    id: 'req-1',
    run_id: 'run-1',
    step_id: 'CONFIRM-FIXES',
    kind: 'feedback',
    title: '选择要整改的问题',
    description: '',
    required_action: 'submit_feedback',
    status: 'pending',
    schema,
  } as any;
}

// 1. 没有 schema：明确回落，不是静默空白。
{
  const form = buildInteractionForm(request(null));
  assert.equal(form.fields.length, 0);
  assert.match(form.fallbackReason, /没有声明提交格式/);
}

// 2. 典型多类型 schema：string / textarea / number / boolean / select。
{
  const form = buildInteractionForm(
    request({
      type: 'object',
      description: '勾选并补充说明',
      properties: {
        selected: { type: 'array', items: { enum: ['IA-1', 'CRAFT-3'] }, title: '要整改的问题' },
        note: { type: 'string', format: 'textarea', title: '补充说明' },
        rounds: { type: 'integer', title: '轮次' },
        confirmed: { type: 'boolean', title: '确认', description: '确认后才会执行' },
        level: { type: 'string', enum: [{ value: 'high', label: '高' }, 'low'], title: '优先级' },
      },
      required: ['selected'],
    }),
  );
  assert.equal(form.fallbackReason, '', '五类常用字段都应能渲染');
  assert.equal(form.fields.length, 5);
  assert.equal(form.fields.find(f => f.name === 'selected')!.kind, 'select');
  assert.equal(form.fields.find(f => f.name === 'note')!.kind, 'textarea');
  assert.equal(form.fields.find(f => f.name === 'rounds')!.kind, 'number');
  assert.equal(form.fields.find(f => f.name === 'confirmed')!.kind, 'boolean');
  assert.equal(form.fields.find(f => f.name === 'selected')!.required, true);
  // label 优先取 title；选项支持 {value,label} 与裸字符串混写。
  assert.deepEqual(form.fields.find(f => f.name === 'level')!.options, [
    { value: 'high', label: '高' },
    { value: 'low', label: 'low' },
  ]);
}

// 3. 无法结构化的字段 → 整体回落，不半渲染。
{
  const form = buildInteractionForm(
    request({
      type: 'object',
      properties: {
        note: { type: 'string' },
        nested: { type: 'array', items: { type: 'object' } },
      },
    }),
  );
  assert.equal(form.fields.length, 0, '有字段推不出类型时不得返回部分字段');
  assert.match(form.fallbackReason, /nested/);
}

// 4. 组合语义（$ref / oneOf）整体回落。
{
  for (const key of ['$ref', 'allOf', 'anyOf', 'oneOf']) {
    const form = buildInteractionForm(request({ type: 'object', [key]: {}, properties: { a: { type: 'string' } } }));
    assert.equal(form.fields.length, 0, `${key} 必须整体回落`);
  }
}

// 5. 初值与必填校验。
{
  const form = buildInteractionForm(
    request({
      type: 'object',
      properties: {
        rounds: { type: 'number', default: 3 },
        confirmed: { type: 'boolean' },
        note: { type: 'string' },
      },
      required: ['note'],
    }),
  );
  const initial = initialFormState(form.fields);
  assert.equal(initial.rounds, 3, '有默认值就用默认值');
  assert.equal(initial.confirmed, false, '布尔字段默认 false 而不是空串');
  assert.equal(initial.note, '');

  const missing = formToFeedback(form.fields, { rounds: 3, confirmed: true, note: '' });
  assert.equal(missing.feedback, '');
  assert.match(missing.error, /补充说明|note/, '必填错误必须指出具体字段');

  const ok = formToFeedback(form.fields, { rounds: 5, confirmed: true, note: '优先修对比度' });
  assert.deepEqual(JSON.parse(ok.feedback), { rounds: 5, confirmed: true, note: '优先修对比度' });
}

// 6. 空值不写进提交内容：避免用空串覆盖服务端默认。
{
  const form = buildInteractionForm(
    request({ type: 'object', properties: { a: { type: 'string' }, b: { type: 'number' } } }),
  );
  const out = formToFeedback(form.fields, { a: '', b: '' });
  assert.deepEqual(JSON.parse(out.feedback), {});
}

// 7. 数字字段给非数字要拦住。
{
  const form = buildInteractionForm(request({ type: 'object', properties: { n: { type: 'number' } } }));
  const out = formToFeedback(form.fields, { n: 'abc' });
  assert.match(out.error, /必须是数字/);
}

console.log('interaction form checks passed');
