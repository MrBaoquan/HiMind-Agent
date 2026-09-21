// 启动表单转换的回归自检（零依赖：node --experimental-strip-types）。
// 覆盖一次真实事故：预设里的列表被 String() 拍成一行 "cv,llm,ar-vr"，
// 运行期才在能力入参校验里报错，而用户只看到「运行失败」。
import { strict as assert } from 'node:assert';
import {
  fieldsToInput,
  initialFormValues,
  invalidOptionValue,
  normalizeStartField,
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

console.log('start form conversion checks passed');
