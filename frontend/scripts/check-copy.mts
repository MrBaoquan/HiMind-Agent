// 用户可见文案的长度与用词约束（零依赖：node --experimental-strip-types）。
//
// 背景：agent 侧的说明型文案一度膨胀——3400+ 条中文串里有 197/268 条市场说明
// 超过 40 字、列表要滚 53 屏；设置页也到处是「用于…」「帮你…」的说明书句式。
// 收敛后的结论只有三条：
//   1. 不写默认行为，只写「改了会怎样」；
//   2. 写后果，不介绍功能；
//   3. 没话说就不渲染，不留「暂无说明」占位。
// 这里把结论固化成可判定的不变量，避免下次又长回去。
import { strict as assert } from 'node:assert';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, relative } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const srcRoot = join(here, '..', 'src');

// 各处的字数预算（按汉字计，标点不算）。超预算说明在解释默认行为或介绍功能。
const BUDGET = {
  pageHeader: 20, // 页头说明：一句话说清这一页能做什么，说完就走
  settingRow: 24, // 设置项说明：只写非默认行为或后果
  emptyState: 40, // 空状态说明：怎么产生第一条数据
  hardCap: 60, // 单条用户可见文案的硬上限，安全/不可逆警告才允许接近
};

// 纯 AI 味的填充语。出现即失败：它们不传递任何新信息。
const FILLERS = ['帮你', '让你', '可以实现', '需要注意的是', '温馨提示', '一站式', '众所周知', '希望对你有帮助', '恭喜你'];

// 空值占位句。没有内容就该直接不渲染，而不是印一句「暂无说明」。
const PLACEHOLDERS = ['暂无说明。', '暂无描述。', '暂无简介。', '未提供更新说明。', '未提供项目说明。', '暂无更新说明。'];

const han = (text: string) => (text.match(/[\u4e00-\u9fa5]/g) || []).length;

// 去掉注释但保留字符串字面量：注释里出现长句是正常的，不该计入文案。
function stripComments(src: string) {
  let out = '';
  let i = 0;
  while (i < src.length) {
    const c = src[i];
    const next = src[i + 1];
    if (c === '/' && next === '/') {
      while (i < src.length && src[i] !== '\n') i++;
    } else if (c === '/' && next === '*') {
      i += 2;
      while (i < src.length && !(src[i] === '*' && src[i + 1] === '/')) {
        if (src[i] === '\n') out += '\n';
        i++;
      }
      i += 2;
    } else if (c === '"' || c === "'" || c === '`') {
      const quote = c;
      out += c;
      i++;
      while (i < src.length && src[i] !== quote) {
        if (src[i] === '\\') {
          out += src[i] + (src[i + 1] ?? '');
          i += 2;
          continue;
        }
        out += src[i];
        i++;
      }
      out += quote;
      i++;
    } else {
      out += c;
      i++;
    }
  }
  return out;
}

function walk(dir: string, acc: string[] = []): string[] {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) walk(full, acc);
    else if (/\.(tsx|ts)$/.test(entry.name)) acc.push(full);
  }
  return acc;
}

// 取一个 JSX 组件元素的开标签（从 `<Name` 到配对的那个 `>`），用来限定预算的适用范围。
function openTags(src: string, component: string): string[] {
  const tags: string[] = [];
  const needle = `<${component}`;
  let i = src.indexOf(needle);
  while (i !== -1) {
    const boundary = src[i + needle.length];
    if (boundary && /[A-Za-z0-9_]/.test(boundary)) {
      i = src.indexOf(needle, i + needle.length);
      continue;
    }
    let j = i + needle.length;
    let depth = 0;
    let quote: string | null = null;
    while (j < src.length) {
      const c = src[j];
      if (quote) {
        if (c === '\\') { j += 2; continue; }
        if (c === quote) quote = null;
        j++;
        continue;
      }
      if (c === '"' || c === "'" || c === '`') { quote = c; j++; continue; }
      if (c === '{') depth++;
      else if (c === '}') depth--;
      else if (c === '>' && depth === 0) break;
      j++;
    }
    if (j >= src.length) break;
    tags.push(src.slice(i, j + 1));
    i = src.indexOf(needle, j);
  }
  return tags;
}

// 读一个属性上的全部字符串字面量。属性可以是 "..."，也可以是 {cond ? '...' : '...'}。
function propLiterals(tag: string, prop: string): string[] {
  const match = new RegExp(`(?:^|\\s)${prop}=`).exec(tag);
  if (!match) return [];
  let i = match.index + match[0].length;
  const first = tag[i];
  if (first === '"' || first === "'") {
    let j = i + 1;
    while (j < tag.length && tag[j] !== first) j++;
    return [tag.slice(i + 1, j)];
  }
  if (first !== '{') return [];
  let depth = 0;
  let quote: string | null = null;
  let j = i;
  while (j < tag.length) {
    const c = tag[j];
    if (quote) {
      if (c === '\\') { j += 2; continue; }
      if (c === quote) quote = null;
      j++;
      continue;
    }
    if (c === '"' || c === "'" || c === '`') { quote = c; j++; continue; }
    if (c === '{') depth++;
    else if (c === '}') { depth--; if (depth === 0) break; }
    j++;
  }
  const expr = tag.slice(i + 1, j);
  return [...expr.matchAll(/'([^'\\]*)'|"([^"\\]*)"|`([^`\\]*)`/g)].map((m) => m[1] ?? m[2] ?? m[3]);
}

const files = walk(srcRoot);
const violations: string[] = [];
const duplicates = new Map<string, { count: number; file: string }>();
const bands = { over40: 0, band28: 0, band18: 0 };
const where = (file: string, text: string, budget: number, reason: string) =>
  violations.push(`${relative(srcRoot, file)} · ${reason}（${han(text)}/${budget} 字）\n    ${text}`);

for (const file of files) {
  const src = stripComments(readFileSync(file, 'utf8'));

  for (const tag of openTags(src, 'PageHeader')) {
    for (const text of propLiterals(tag, 'description')) {
      if (han(text) > BUDGET.pageHeader) where(file, text, BUDGET.pageHeader, '页头说明超出预算');
    }
  }
  for (const tag of openTags(src, 'SettingRow')) {
    for (const text of propLiterals(tag, 'description')) {
      if (han(text) > BUDGET.settingRow) where(file, text, BUDGET.settingRow, '设置项说明超出预算');
      for (const filler of FILLERS) if (text.includes(filler)) where(file, text, BUDGET.settingRow, `设置项说明含填充语「${filler}」`);
    }
  }
  for (const tag of openTags(src, 'EmptyState')) {
    for (const text of propLiterals(tag, 'text')) {
      if (han(text) > BUDGET.emptyState) where(file, text, BUDGET.emptyState, '空状态说明超出预算');
    }
  }

  // 全量扫描：单条文案的硬上限、填充语、占位句、重复文案。
  for (const match of src.matchAll(/[\u4e00-\u9fa5][^'"<>`{}\n]{2,200}/g)) {
    const text = match[0].trim();
    const length = han(text);
    if (length >= 40) bands.over40++;
    else if (length >= 28) bands.band28++;
    else if (length >= 18) bands.band18++;
    if (length > BUDGET.hardCap) where(file, text, BUDGET.hardCap, '单条文案超出硬上限');
    for (const filler of FILLERS) if (text.includes(filler)) where(file, text, BUDGET.hardCap, `含填充语「${filler}」`);
    for (const placeholder of PLACEHOLDERS) if (text.includes(placeholder)) where(file, text, BUDGET.hardCap, `空值占位句「${placeholder}」`);
    if (length >= 12) {
      const seen = duplicates.get(text);
      if (seen) seen.count++;
      else duplicates.set(text, { count: 1, file: relative(srcRoot, file) });
    }
  }
}

assert.equal(violations.length, 0, `文案约束未通过（${violations.length} 条）：\n  - ${violations.join('\n  - ')}`);

// 增长天花板：当前 20 条 / 0 条，留出余量但拦住成规模的文案膨胀。
assert.ok(bands.over40 === 0, `不得出现超过 40 字的用户可见文案，当前 ${bands.over40} 条`);
assert.ok(bands.band28 <= 28, `28–39 字的文案不得超过 28 条，当前 ${bands.band28} 条：说明又在解释默认行为`);

const repeated = [...duplicates.entries()].filter(([, value]) => value.count > 1);
if (repeated.length) {
  console.warn(`提示：${repeated.length} 条文案在多处重复，确认是否有必要逐处重写。`);
  for (const [text, value] of repeated.slice(0, 5)) console.warn(`  ×${value.count} ${text}`);
}

console.log(`copy checks passed（${files.length} 个文件 / 40+ 字 ${bands.over40} 条 / 28+ 字 ${bands.band28} 条）`);
