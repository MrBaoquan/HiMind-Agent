import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
  compatibleEntryProtocolsForUpstream,
  compatibleUpstreamProtocols,
  gatewayCompatibilityMessage,
  supportedUpstreamProtocolsForEntry,
  gatewayProfileProtocols,
  serviceSupportsGatewayProtocols,
  setGatewayEntryProtocol,
  visibleProfileModels,
} from '../src/pages/gatewayProfileView.ts';
import type { AIServiceProtocol, GatewayModelRoute } from '../src/services/agentApi.ts';

const allEntries: AIServiceProtocol[] = ['openai-chat', 'openai-responses', 'anthropic'];
assert.deepEqual(compatibleUpstreamProtocols(['openai-chat']), ['openai-chat']);
assert.deepEqual(compatibleUpstreamProtocols(['openai-responses']), ['openai-responses', 'openai-chat']);
assert.deepEqual(compatibleUpstreamProtocols(['anthropic']), ['openai-responses', 'anthropic', 'openai-chat']);
assert.deepEqual(compatibleUpstreamProtocols(allEntries), ['openai-chat']);
assert.match(gatewayCompatibilityMessage(allEntries), /至少保留一个启用模型/);

const responsesAndAnthropic = setGatewayEntryProtocol(['openai-responses'], 'anthropic', true);
assert.deepEqual(responsesAndAnthropic, ['openai-responses', 'anthropic']);
assert.deepEqual(setGatewayEntryProtocol(responsesAndAnthropic, 'openai-chat', true), allEntries);
assert.deepEqual(setGatewayEntryProtocol(['anthropic'], 'anthropic', false), ['anthropic'],
  '最后一个客户端协议不能被取消');
assert.deepEqual(gatewayProfileProtocols({ entry_protocol: 'anthropic', entry_protocols: [] }), ['anthropic'],
  '旧 Profile 的 entry_protocol 必须继续可读');

assert.equal(serviceSupportsGatewayProtocols({ protocol: 'openai-chat' }, responsesAndAnthropic), true);
assert.equal(serviceSupportsGatewayProtocols({ protocol: 'anthropic' }, responsesAndAnthropic), true);
assert.equal(serviceSupportsGatewayProtocols({ protocol: 'openai-responses' }, responsesAndAnthropic), true);
assert.deepEqual(compatibleEntryProtocolsForUpstream('anthropic', responsesAndAnthropic), ['anthropic']);
assert.deepEqual(compatibleEntryProtocolsForUpstream('openai-responses', responsesAndAnthropic), ['openai-responses', 'anthropic']);
assert.deepEqual(supportedUpstreamProtocolsForEntry('anthropic'), ['openai-responses', 'anthropic', 'openai-chat']);

const routes: GatewayModelRoute[] = Array.from({ length: 5 }, (_, index) => ({
  exposed_model: 'service/model-' + index,
  service_id: 'service',
  upstream_model: 'model-' + index,
  enabled: true,
}));
assert.equal(visibleProfileModels(routes, false).length, 3, '卡片默认最多展示 3 个模型');
assert.equal(visibleProfileModels(routes, true).length, 5, '展开后必须能查看全部模型');

const panel = readFileSync(new URL('../src/pages/GatewayProfilesPanel.tsx', import.meta.url), 'utf8');
assert.match(panel, /entry_protocols/, '编辑器必须提交多协议字段');
assert.match(panel, /type="checkbox"/, '协议选择必须使用带文字的原生复选控件');
assert.match(panel, /role="status"/, '协议兼容性预览需要可感知状态');
assert.match(panel, /gateway-route-field-label/, '窄窗路由字段必须有自身标签');
assert.match(panel, /gateway-protocol-coverage/, '协议预览必须显示每个客户端可用的上游');
assert.match(panel, /gateway-route-clients/, '模型行必须显示会注册到哪些客户端');
assert.match(panel, /aria-busy/, '保存、启停和删除需要单项进行中状态');
assert.doesNotMatch(panel, /同一入口可以同时接受|Codex 使用 Responses|AI 工具即可在同一入口切换模型/, '模型网关首屏不能再堆叠重复说明');
assert.doesNotMatch(panel, /别名必须在本入口内唯一|先选择客户端协议，再添加兼容的模型/, '模型区文案应收敛为状态和动作');
assert.match(panel, /暂无模型/, '模型空态只保留必要结果，不重复操作教程');

const styles = readFileSync(new URL('../styles.css', import.meta.url), 'utf8');
assert.doesNotMatch(styles, /\.gateway-profile-card\.disabled\s*\{[^}]*opacity/, '停用卡片不能靠整体降透明度损害文字对比度');
const gatewayStylesStart = styles.indexOf('/* 模型网关');
const mobile1180 = styles.indexOf('@media (max-width: 1180px)', gatewayStylesStart);
const mobile620 = styles.indexOf('@media (max-width: 620px)');
assert.ok(gatewayStylesStart >= 0 && mobile1180 > gatewayStylesStart && mobile620 > mobile1180, '必须提供 1180px 与 620px 两级响应式约束');
const mobileBlock = styles.slice(mobile1180, mobile620);
assert.match(mobileBlock, /\.gateway-route-field-label\s*\{[^}]*display:\s*block/, '1180px 以下必须显示路由字段标签');
const narrowBlock = styles.slice(mobile620);
assert.match(narrowBlock, /\.gateway-profile-form-row\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\)/, '620px 以下表单单列');
assert.match(narrowBlock, /\.gateway-route-edit-row\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\)/, '620px 以下模型路由单列');

const enabledControlRule = styles.match(/\.gateway-profile-enabled > span:last-child \{[^}]*\}/);
assert.ok(enabledControlRule && /min-height:\s*36px/.test(enabledControlRule[0]), '入口状态必须与相邻表单控件保持 36px 高度');
assert.match(styles, /\.gateway-profile-enabled input\[type='checkbox'\]\s*\{[^}]*width:\s*16px[^}]*height:\s*16px[^}]*margin:\s*0/, '入口状态 checkbox 必须有确定尺寸且清除浏览器默认外边距');
assert.match(styles, /\.gateway-profile-enabled > span:last-child:focus-within\s*\{[^}]*box-shadow/, '入口状态控件必须有清晰可见的键盘焦点环');

assert.match(styles, /\.ai-tabs\s*\{\s*flex-wrap:\s*wrap/, '极窄窗口下 AI 连接页签必须换行，避免末项被横向滚动隐藏');
assert.match(styles, /\.ai-binding-filter\s*\{[^}]*width:\s*100%/, '极窄窗口下 AI 工具筛选器必须换行到独立整行');
console.log('gateway profile UI checks passed');
