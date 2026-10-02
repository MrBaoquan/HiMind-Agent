// 内置 AI 服务预设的自检（零依赖：node --experimental-strip-types）。
//
// 这些预设在独立模式下会被直接填进「新增 AI 服务」表单，一旦写错就是用户拿到
// 一个连不上的服务：接入地址必须是可公开访问的 https 主机、协议必须是本机
// 认识的三种之一、默认模型必须在模型列表里。文案同样有约束——预设说明是
// 列表里的一行小字，长了就会被截断成一句 AI 味十足的广告词。
import { strict as assert } from 'node:assert';
import { readFileSync } from 'node:fs';
import { fallbackAiServicePresets } from '../src/pages/aiServicePresets.ts';

const protocols = new Set(['openai-chat', 'openai-responses', 'anthropic']);
const ids = new Set<string>();

assert.ok(fallbackAiServicePresets.length >= 5, '内置预设太少，独立模式下用户加不上常用服务');

for (const preset of fallbackAiServicePresets) {
  const where = `预设 ${preset.id || '(空 id)'}`;

  assert.match(preset.id, /^[a-z0-9][a-z0-9_-]*$/, `${where}：id 只能是小写字母、数字、下划线和短横线`);
  assert.ok(!ids.has(preset.id), `${where}：id 重复`);
  ids.add(preset.id);

  assert.ok(preset.name.trim().length > 0, `${where}：缺少名称`);
  assert.ok(preset.name.length <= 20, `${where}：名称过长（${preset.name.length} 字）`);
  assert.ok(preset.category.trim().length > 0, `${where}：缺少分类，预设列表会掉进「其他」`);
  assert.ok(preset.category.length <= 10, `${where}：分类过长（${preset.category.length} 字）`);
  assert.ok(preset.description.trim().length > 0, `${where}：缺少说明`);
  // 说明在卡片里最多占两行（约 14 个字一行），超过 28 字会被行数截断，等于没写。
  assert.ok(preset.description.length <= 28, `${where}：说明过长（${preset.description.length} 字），卡片两行放不下`);

  assert.ok(protocols.has(preset.protocol), `${where}：协议 ${preset.protocol} 不是本机支持的协议`);
  assert.ok(preset.base_url.startsWith('https://'), `${where}：接入地址必须是 https`);
  assert.ok(!preset.base_url.endsWith('/'), `${where}：接入地址不要带结尾斜杠`);
  assert.ok(!/localhost|127\.0\.0\.1|:\d+$/.test(preset.base_url), `${where}：内置预设只能指向公网厂商地址`);
  const host = preset.base_url.slice('https://'.length).split('/')[0];
  assert.ok(host.includes('.') && !host.includes(' '), `${where}：接入地址主机名不合法`);

  assert.ok(preset.models.length > 0, `${where}：模型列表为空，表单会留白给用户猜`);
  assert.equal(new Set(preset.models).size, preset.models.length, `${where}：模型列表有重复项`);
  for (const model of preset.models) {
    assert.ok(model.trim() === model && model.length > 0, `${where}：模型 ID「${model}」含空白`);
  }
  assert.ok(
    preset.models.includes(preset.default_model),
    `${where}：默认模型 ${preset.default_model} 不在模型列表里`,
  );
}

// 分类是预设列表的分组标签，至少要覆盖「国际」「国内」这种用户能一眼分辨的划分。
const categories = new Set(fallbackAiServicePresets.map((preset) => preset.category));
assert.ok(categories.size >= 2, '内置预设至少要分成两组，否则分类页签没有意义');

// 每家厂商都要有说明可读的默认模型：空说明或空默认模型等于预设没填完。
assert.equal(
  fallbackAiServicePresets.filter((preset) => !preset.default_model).length,
  0,
  '存在没有默认模型的预设',
);

console.log(`ai service preset checks passed（${fallbackAiServicePresets.length} 条 / ${categories.size} 个分类）`);

// 预设卡片的排版不变量：目录里的名称与说明长度不受前端控制，样式必须自己扛住。
// 症状记录：说明写成一行 nowrap + 220px 上限时，「腾讯云混元 Token Plan 企业版专属接入点」
// 会被截成「…专属接…」，同一行的卡片宽度也会跟着文字长短参差。
const styles = readFileSync(new URL('../styles.css', import.meta.url), 'utf8');
const rule = (selector: string) => {
  const match = new RegExp(`\\${selector}\\s*\\{([^}]*)\\}`).exec(styles);
  assert.ok(match, `styles.css 缺少 ${selector} 规则`);
  return match![1].replace(/\s+/g, ' ');
};

const listRule = rule('.ai-service-preset-list');
assert.match(listRule, /display: grid/, '.ai-service-preset-list 需要用网格布局，卡片列才能对齐');
assert.match(listRule, /grid-template-columns: repeat\(auto-fill, minmax\(\d+px, 1fr\)\)/, '预设列表要自适应列数，窄窗口下不能靠固定列数硬撑');

const chipRule = rule('.ai-service-preset-chip');
assert.ok(!/max-width:\s*\d+px/.test(chipRule), '预设卡片不能再按文字宽度设上限，说明会被截断');
// 全局 `button { white-space: nowrap }` 会被卡片继承，名称与说明就都换不了行。
assert.match(chipRule, /white-space: normal/, '预设卡片必须显式改回 normal，否则继承按钮的 nowrap，说明只显示一行后被横向裁掉');

const chipTextRule = rule('.ai-service-preset-chip span');
assert.ok(!/white-space: nowrap/.test(chipTextRule), '预设说明不能强制单行，长了必须换行');
assert.match(chipTextRule, /-webkit-line-clamp: 2/, '预设说明最多显示两行');
assert.match(chipTextRule, /overflow: hidden/, '预设说明超出两行时要截断，不能溢出卡片');

console.log('ai service preset layout checks passed');
