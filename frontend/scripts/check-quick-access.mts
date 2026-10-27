import { strict as assert } from 'node:assert';
import { readFileSync } from 'node:fs';
import {
  dedupeQuickAccessViews,
  pickUniqueQuickAccessLabels,
  quickAccessLabel,
} from '../src/data/quickAccess.ts';

const base = {
  plugin_name: '自媒体创作台',
  title: '自媒体创作台',
  short_title: '创作台',
  icon: 'monitor',
  order: 1,
};
const views = [
  { ...base, plugin_id: 'p1', view_id: 'v1' },
  { ...base, plugin_id: 'p1', view_id: 'v1' },
  { ...base, plugin_id: 'p1', view_id: 'v2' },
  { ...base, plugin_id: 'p2', view_id: 'v1' },
  { ...base, plugin_id: 'p2', view_id: 'v2', short_title: '日志', title: '工作日志', order: 2 },
];

const deduped = dedupeQuickAccessViews(views);
assert.equal(deduped.length, 3, '同一插件下重复目标或重复标签只能保留一个入口');
assert.equal(deduped.filter(view => view.plugin_id === 'p1').length, 1);
assert.equal(deduped.filter(view => view.plugin_id === 'p2').length, 2, '不同插件的同名工具仍需保留在全部工具中');
assert.deepEqual(
  pickUniqueQuickAccessLabels(deduped, 4).map(quickAccessLabel),
  ['创作台', '日志'],
  '顶部工具栏无论来源插件如何，都不能出现两个同名入口',
);
assert.equal(pickUniqueQuickAccessLabels(deduped, 1).length, 1, '顶部工具栏仍受可见数量上限约束');

const styles = readFileSync(new URL('../styles.css', import.meta.url), 'utf8');
assert.match(styles, /\.quick-access-dialog-search input\s*\{[^}]*appearance: none/,
  '全部工具搜索框要关闭浏览器原生 search 外观');
const focusBlock = [...styles.matchAll(/([^{}]+)\{([^{}]*)\}/g)]
  .find(match => match[1].includes('.quick-access-dialog-search input:focus'));
assert.ok(focusBlock, '全部工具搜索框必须加入统一的内层焦点环清除规则');
assert.match(focusBlock![2], /box-shadow: none/, '焦点环只能由搜索框外壳绘制');
console.log('quick access checks passed');
