// 列表说明与路径的排版不变量（零依赖：node --experimental-strip-types）。
//
// 这两类文本的特点是内容不受前端控制：说明来自目录里的 description，路径来自
// 用户机器的实际安装位置。它们都曾栽在同一个坑里——为了单行对齐写了
// `white-space: nowrap`，结果整句话或整条路径被横向裁掉：
//   · 市场列表 23/23 条说明只显示前半句（可视 291px / 实际 623px）；
//   · 设置 → AI 连接的配置路径被切掉文件名（可视 587px / 实际 714px）。
// 这里把「说明最多两行、路径整段可读」固化成可判定的顺序不变量：覆盖规则必须
// 出现在单行省略规则之后，否则同权重下先写的 nowrap 会赢。
import { strict as assert } from 'node:assert';
import { readFileSync } from 'node:fs';

// 注释里常引用带大括号的写法（例如 `button { white-space: nowrap }`），先去掉，
// 否则注释会被当成下一条规则的选择器。
const styles = readFileSync(new URL('../styles.css', import.meta.url), 'utf8')
  .replace(/\/\*[\s\S]*?\*\//g, '');

type Block = { selector: string; body: string; index: number };
const blocks: Block[] = [...styles.matchAll(/([^{}]+)\{([^{}]*)\}/g)].map((match) => ({
  selector: match[1].trim(),
  body: match[2].replace(/\s+/g, ' ').trim(),
  index: match.index ?? 0,
}));

const mentions = (block: Block, needle: string) => block.selector.includes(needle);
const lastBlock = (predicate: (block: Block) => boolean) => [...blocks].reverse().find(predicate);

// 13. 说明句两行：覆盖规则必须在所有单行省略规则之后，并且显式改回 normal。
const descriptionOverride = lastBlock((block) =>
  mentions(block, '.skill-browser-item-copy > small:last-of-type'),
);
assert.ok(descriptionOverride, 'styles.css 缺少列表说明的两行规则（.skill-browser-item-copy > small:last-of-type）');
assert.match(descriptionOverride!.selector, /\.market-item-copy > small:not\(\.catalog-item-author\)/,
  '市场列表的说明容器也要纳入同一组规则，否则只有技能列表能换行');
assert.match(descriptionOverride!.selector, /\.inbox-item-main > span/,
  '待处理列表的说明容器也要纳入同一组规则，否则长说明会被裁成单行');
assert.match(descriptionOverride!.body, /-webkit-line-clamp: 2/, '列表说明最多显示两行');
assert.match(descriptionOverride!.body, /-webkit-box-orient: vertical/, '行数截断需要 -webkit-box-orient: vertical 配合');
assert.match(descriptionOverride!.body, /overflow: hidden/, '列表说明超出两行要截断，不能溢出列表项');
assert.match(descriptionOverride!.body, /white-space: normal/,
  '列表项是 button，会继承全局 nowrap；不显式写回 normal 时 -webkit-line-clamp 不生效');
assert.match(descriptionOverride!.body, /overflow-wrap: anywhere/, '说明里的长串（ID、URL）要能断词');

// 覆盖规则之后不允许再出现针对这些列表项的单行省略，否则顺序反了就是又裁一次。
const laterNowrap = blocks.filter(
  (block) =>
    block.index > descriptionOverride!.index &&
    /white-space: nowrap/.test(block.body) &&
    /\.(market-item|market-item-copy|skill-browser-item-copy|inbox-item-main)\b/.test(block.selector),
);
assert.equal(laterNowrap.length, 0,
  `说明两行规则之后又出现了单行省略规则，会重新裁掉说明：${laterNowrap.map((block) => block.selector).join(' / ')}`);

// 14. 工具名不再被挤到 72px 以下：单列宽度下限要容得下最长工具名（GitHub Copilot ≈ 79px）。
const toolGrid = lastBlock((block) => block.selector === '.skill-client-availability > details > div');
assert.ok(toolGrid, 'styles.css 缺少投放目标工具网格规则');
const minTrack = /minmax\((\d+)px, 1fr\)/.exec(toolGrid!.body);
assert.ok(minTrack, '投放目标工具网格要用 auto-fit + minmax 自适应列数');
assert.ok(
  Number(minTrack![1]) >= 172,
  `投放目标网格单列下限 ${minTrack![1]}px 太窄：名称列只剩不到 79px，「GitHub Copilot」会被裁字`,
);
const toolName = lastBlock((block) => block.selector.includes('.skill-client-tool-copy strong'));
assert.ok(toolName && /overflow-wrap: anywhere/.test(toolName.body), '工具名要允许折词，超长名称不能被切掉半截');

// 15. 路径整段可读：这组选择器最后一次出现时都不能再强制单行。
const pathSelectors = [
  '.ai-diagnostic-path code',
  '.runtime-facts code',
  '.skill-target-path',
  '.plan-target-path',
  '.skill-project-sources code',
  '.managed-summary-note > code',
];
for (const selector of pathSelectors) {
  const block = lastBlock((candidate) => candidate.selector.split(',').map((part) => part.trim()).includes(selector));
  assert.ok(block, `styles.css 缺少 ${selector} 规则`);
  assert.doesNotMatch(block!.body, /white-space: nowrap/,
    `${selector} 仍被强制单行：路径被截掉的往往是文件名，用户照路径找不到文件`);
  assert.match(block!.body, /overflow-wrap: anywhere/, `${selector} 需要断词，长路径才能整段显示`);
}

console.log(`text layout checks passed（${blocks.length} 条规则 / ${pathSelectors.length} 组路径）`);
