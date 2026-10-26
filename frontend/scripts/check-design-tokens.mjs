import fs from 'node:fs';

const css = fs.readFileSync(new URL('../styles.css', import.meta.url), 'utf8');
const failures = [];
const vars = new Map();
for (const block of css.matchAll(/:root\s*\{([^}]*)\}/g)) {
  for (const match of block[1].matchAll(/--([\w-]+)\s*:\s*([^;]+);/g)) vars.set(match[1], match[2].trim());
}

const required = [
  'primary', 'primary-hover', 'primary-strong', 'primary-soft', 'primary-border',
  'success', 'success-soft', 'warning', 'warning-soft', 'danger',
  'bg', 'surface', 'surface-muted', 'border', 'border-strong', 'control-border', 'neutral-soft',
  'text', 'text-muted', 'text-subtle', 'focus-ring',
  'fs-meta', 'fs-body', 'fs-title',
];
for (const name of required) if (!vars.has(name)) failures.push(`缺少令牌 --${name}`);

function resolve(name) {
  let value = vars.get(name);
  for (let i = 0; i < 4 && value; i += 1) {
    const match = value.match(/^var\(--([\w-]+)\)$/);
    if (!match) break;
    value = vars.get(match[1]);
  }
  return value;
}
function rgb(name) {
  const value = resolve(name);
  const match = value?.match(/^#([0-9a-f]{3}|[0-9a-f]{6})$/i);
  if (!match) throw new Error(`令牌 --${name} 不是可测的十六进制颜色: ${value}`);
  const hex = match[1].length === 3 ? match[1].split('').map(char => char + char).join('') : match[1];
  return [0, 2, 4].map(index => Number.parseInt(hex.slice(index, index + 2), 16));
}
function luminance([r, g, b]) {
  const channel = value => {
    const normalized = value / 255;
    return normalized <= 0.04045 ? normalized / 12.92 : ((normalized + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}
function contrast(left, right) {
  const a = luminance(rgb(left));
  const b = luminance(rgb(right));
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}
function expectContrast(left, right, minimum) {
  const ratio = contrast(left, right);
  if (ratio + 0.01 < minimum) failures.push(`--${left} on --${right} = ${ratio.toFixed(2)}:1，要求 ≥${minimum}:1`);
  return ratio;
}

for (const background of ['surface', 'bg', 'surface-muted']) expectContrast('text', background, 4.5);
for (const background of ['surface', 'surface-muted']) expectContrast('control-border', background, 3);
for (const background of ['surface', 'bg', 'surface-muted', 'primary-soft']) expectContrast('text-muted', background, 4.5);
for (const background of ['surface', 'bg', 'surface-muted', 'neutral-soft']) expectContrast('text-subtle', background, 4.5);
expectContrast('primary', 'surface', 4.5);
expectContrast('success', 'success-soft', 4.5);
expectContrast('warning', 'warning-soft', 4.5);
expectContrast('danger', 'bg', 4.5);

const metaSize = Number.parseFloat(resolve('fs-meta') || '0');
const bodySize = Number.parseFloat(resolve('fs-body') || '0');
const titleSize = Number.parseFloat(resolve('fs-title') || '0');
if (metaSize < 12) failures.push(`--fs-meta=${metaSize}px，要求 ≥12px`);
if (bodySize < 13) failures.push(`--fs-body=${bodySize}px，要求 ≥13px`);
if (titleSize < 14) failures.push(`--fs-title=${titleSize}px，要求 ≥14px`);

const focusRule = css.match(/button:focus-visible,\s*input:focus-visible,\s*select:focus-visible\s*\{([^}]*)\}/)?.[1] || '';
if (!focusRule.includes('var(--primary)') || /rgba\(37,\s*99,\s*235,\s*0\.35\)/.test(focusRule)) {
  failures.push('全局输入焦点样式必须使用不透明 var(--primary)，不得保留 0.35 透明度覆盖');
}
if (!/\.settings-rail-search-clear\s*\{[^}]*width:\s*24px; height:\s*24px/.test(css)) failures.push('设置搜索清除按钮命中区必须 ≥24×24');
if (!/\.settings-window-root \.credential-center-search[\s\S]*?border-color:\s*var\(--control-border\)/.test(css)) failures.push('账号搜索框必须使用可达到 3:1 的 --control-border');
if (!/--focus-ring:\s*0 0 0 1px var\(--surface\),\s*0 0 0 4px var\(--primary\)/.test(css)) {
  failures.push('--focus-ring 必须使用白色隔离环 + 实色主色外环');
}
if (/--primary:\s*#2563eb/i.test(css)) failures.push('主色仍是旧的亮蓝 #2563eb');
if (!/@media\s*\(prefers-reduced-motion:\s*reduce\)/.test(css)) failures.push('缺少 prefers-reduced-motion: reduce 降级');
if (!/\.text-action\s*\{\s*min-height:\s*24px/.test(css)) failures.push('.text-action 最小命中区必须 ≥24px');
if (!/\.market-update-chip,[^{]*\.market-update-action\s*\{\s*min-height:\s*26px/.test(css)) failures.push('市场更新动作最小命中区必须 ≥26px');

const report = {
  tokens: required.length,
  contrast: {
    bodyOnSurface: Number(contrast('text', 'surface').toFixed(2)),
    mutedOnSurface: Number(contrast('text-muted', 'surface').toFixed(2)),
    subtleOnMuted: Number(contrast('text-subtle', 'surface-muted').toFixed(2)),
    neutralBadge: Number(contrast('text-subtle', 'neutral-soft').toFixed(2)),
    primaryOnSurface: Number(contrast('primary', 'surface').toFixed(2)),
    controlBorderOnSurface: Number(contrast('control-border', 'surface').toFixed(2)),
    controlBorderOnMuted: Number(contrast('control-border', 'surface-muted').toFixed(2)),
  },
  type: { meta: metaSize, body: bodySize, title: titleSize },
};
console.log(JSON.stringify(report, null, 2));
if (failures.length) {
  console.error('设计令牌检查失败：');
  for (const failure of failures) console.error(`- ${failure}`);
  process.exit(1);
}
console.log('design token checks passed');
