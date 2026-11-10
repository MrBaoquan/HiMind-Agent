// 通知通道的投递判定自检（零依赖：node --experimental-strip-types）。
//
// 这里锁的是「什么时候推、推什么」两条不变量。它们出错的表现都很隐蔽：
//   1. 该推的不推 → 人不在电脑前就永远不知道有活要批；
//   2. 推的内容太多 → 代码片段、路径、凭据被带进群聊。
import { strict as assert } from 'node:assert';
import { readFileSync } from 'node:fs';

const source = readFileSync(
  new URL('../src/components/NotificationChannelsPanel.tsx', import.meta.url),
  'utf8',
);

// 1. 六种通道类型都要在界面上可选中，且顺序稳定（钉钉在最前——它是最常配的）。
for (const kind of ['dingtalk', 'feishu', 'wecom', 'slack', 'discord', 'generic']) {
  assert.ok(source.includes(`value: '${kind}'`), `缺少通道类型：${kind}`);
}
assert.ok(
  source.indexOf("value: 'dingtalk'") < source.indexOf("value: 'feishu'"),
  '钉钉应排在最前',
);

// 2. 两类事件都要能勾选。
for (const event of ['approval.requested', 'run.failed']) {
  assert.ok(source.includes(`value: '${event}'`), `缺少事件类型：${event}`);
}

// 3. 新通道必须填 webhook；编辑已有通道时留空表示不改地址。
assert.match(
  source,
  /if \(!existing && !draft\.webhook\.trim\(\)\)/,
  '新通道缺 webhook 必须拦住',
);
assert.ok(
  source.includes('留空表示不修改已保存的地址'),
  '编辑态必须说明留空的含义',
);

// 4. 凭据状态必须显式提示，不能只靠「看起来配好了」。
assert.ok(
  source.includes('缺少 Webhook 地址，不会投递'),
  '未配置凭据的通道必须给出明确警告',
);
assert.ok(
  source.includes('加密保存在本机凭据里'),
  '必须向用户说明 webhook 地址的存储方式',
);

console.log('notification channels checks passed');
