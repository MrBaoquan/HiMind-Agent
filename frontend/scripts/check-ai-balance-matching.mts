import assert from 'node:assert/strict';
import { inferBalanceProfile } from '../src/pages/balanceProfileMatching.ts';

const cases: Array<[string, string]> = [
  ['https://api.deepseek.com/v1', 'deepseek'],
  ['https://api.moonshot.cn/anthropic', 'kimi'],
  ['https://api.kimi.com/coding', 'kimi-coding'],
  ['https://open.bigmodel.cn/api/paas/v4', 'zhipu-plan'],
  ['https://api.z.ai/api/paas/v4', 'zhipu-plan'],
  ['https://api.minimaxi.com/v1', 'minimax-plan-cn'],
  ['https://api.minimax.cn/v1', 'minimax-plan-cn'],
  ['https://api.minimax.io/v1', 'minimax-plan-en'],
  ['https://open.bigmodel.cn/api/paas/v4', 'zhipu-plan'],
  ['https://api.z.ai/api/paas/v4', 'zhipu-plan'],
  ['https://api.minimaxi.com/v1', 'minimax-plan-cn'],
  ['https://api.minimax.io/v1', 'minimax-plan-en'],
  ['https://api.openrouter.ai/api/v1', 'openrouter'],
  ['https://api.siliconflow.cn/v1', 'siliconflow'],
  ['https://api.siliconflow.com/v1', 'siliconflow-en'],
  ['https://api.stepfun.com/v1', 'stepfun'],
  ['https://api.novita.ai/v3', 'novita'],
  ['https://opencode.ai/zen/go/v1', 'opencode-go'],
];
for (const [url, expected] of cases) {
  assert.equal(inferBalanceProfile(url), expected, `${url} should recommend ${expected}`);
}
for (const url of [
  'https://api.kimi.com.attacker.example/coding',
  'https://open.bigmodel.cn.attacker.example/api/paas/v4',
  'https://open.bigmodel.cn.attacker.example/api/paas/v4',
  'https://opencode.ai/zen/v1',
  'https://openrouter.ai.attacker.example/v1',
  'http://api.openrouter.ai/v1',
  'https://proxy.example/v1',
  'not a url',
]) {
  assert.equal(inferBalanceProfile(url), null, `${url} should not auto-match`);
}
console.log('AI balance template matching checks passed');
