import assert from 'node:assert/strict';
import { AI_CONNECTIONS_TAB_LABELS, AI_CONNECTIONS_TAB_ORDER } from '../src/pages/aiConnectionsTabs.ts';
import {
  capabilityModel,
  capabilityModels,
  inferPresetCapabilities,
  primaryRouteModel,
  primaryRouteServiceId,
  resolveMediaRoute,
  serviceCapabilities,
  serviceSupportsCapability,
  suggestedServiceId,
} from '../src/pages/aiServiceCapabilities.ts';
import { buildToolConnections } from '../src/pages/aiToolConnections.ts';
import type { CustomAIService, McpTargetDescriptor } from '../src/services/agentApi.ts';

function service(
  id: string,
  model = '',
  media: Record<string, { adapter: string; model: string; models?: string[] }> = {},
  models: string[] = model ? [model] : [],
): CustomAIService {
  return {
    id,
    display_name: id,
    base_url: `https://${id}.example/v1`,
    protocol: 'openai-responses',
    model,
    models,
    image_model: '',
    image_adapter: '',
    media,
    created_at: '',
    updated_at: '',
  } as unknown as CustomAIService;
}

assert.deepEqual(AI_CONNECTIONS_TAB_ORDER, ['mcp', 'services', 'gateway', 'routes', 'acp']);
assert.equal(AI_CONNECTIONS_TAB_LABELS.services, 'AI 服务');
assert.equal(AI_CONNECTIONS_TAB_LABELS.gateway, '模型网关');
assert.equal(AI_CONNECTIONS_TAB_LABELS.routes, '能力路由');

const imageOnly = service('image-only', '', {
  'image.generate': { adapter: 'openai-images', model: 'gpt-image-1' },
});
const videoOnly = service('video-only', '', {
  'video.generate': { adapter: 'ark-video', model: 'seedance-test' },
});
const chatOnly = service('chat-only', 'chat-model');
const legacyVideo = service('legacy-video', 'seedance-legacy', {
  'video.generate': { adapter: 'ark-video', model: 'seedance-legacy' },
});
assert.deepEqual(serviceCapabilities(imageOnly), ['image.generate']);
assert.deepEqual(serviceCapabilities(videoOnly), ['video.generate']);
assert.deepEqual(serviceCapabilities(legacyVideo), ['video.generate']);
assert.deepEqual(serviceCapabilities(chatOnly), ['chat', 'vision.describe']);

// 模型目录：默认模型在前，其余去重后按声明顺序。界面下拉与后端
// `capability_models` 必须给出同一个列表，否则会展示路由不会用的模型。
const multiChat = service('multi-chat', 'chat-a', {}, ['chat-b', 'chat-a', 'chat-c']);
assert.deepEqual(capabilityModels(multiChat, 'chat'), ['chat-a', 'chat-b', 'chat-c']);
assert.equal(capabilityModel(multiChat, 'chat'), 'chat-a');

const multiImage = service('multi-image', '', {
  'image.generate': { adapter: 'ark-images', model: 'seedream-a', models: ['seedream-b', 'seedream-a'] },
});
assert.deepEqual(capabilityModels(multiImage, 'image.generate'), ['seedream-a', 'seedream-b']);
assert.equal(capabilityModel(multiImage, 'image.generate'), 'seedream-a');

// vision 没有显式绑定时跟随对话模型；显式绑定时以绑定为准。
assert.deepEqual(capabilityModels(multiChat, 'vision.describe'), ['chat-a', 'chat-b', 'chat-c']);
const explicitVision = service('vision', 'chat-a', {
  'vision.describe': { adapter: '', model: 'vision-a' },
});
assert.deepEqual(capabilityModels(explicitVision, 'vision.describe'), ['vision-a']);

// 旧数据把媒体模型写进 `model` 且没有别的对话模型：不能当成对话候选，
// 否则打标模型会被当聊天模型发出去（前后端必须同一判定）。
const mediaOnlyChat = service('media-only', 'seedance-legacy', {
  'video.generate': { adapter: 'ark-video', model: 'seedance-legacy' },
});
assert.equal(serviceSupportsCapability(mediaOnlyChat, 'chat'), false);
const chatWithImage = service('chat-with-image', 'gpt-4.1', {
  'image.generate': { adapter: 'openai-images', model: 'gpt-image-1' },
}, ['gpt-4.1']);
assert.deepEqual(capabilityModels(chatWithImage, 'chat'), ['gpt-4.1']);
// 全局 `models` 里的模型照原样是对话候选（与后端一致）：它来自服务自己的模型列表。
const chatListingImageModel = service('chat-listing-image', 'gpt-4.1', {
  'image.generate': { adapter: 'openai-images', model: 'gpt-image-1' },
}, ['gpt-4.1', 'gpt-image-1']);
assert.deepEqual(capabilityModels(chatListingImageModel, 'chat'), ['gpt-4.1', 'gpt-image-1']);

// 主选取值只有一条规则：路由表下标 0（ADR 0023）。备用项与空表都不能被当成主选。
const routeTable = {
  chat: [{ service_id: 'chat-only', model: 'chat-model' }, { service_id: 'image-only', model: '' }],
  'image.generate': [{ service_id: 'image-only', model: '' }],
};
assert.equal(primaryRouteServiceId(routeTable, 'chat'), 'chat-only');
assert.equal(primaryRouteModel(routeTable, 'chat'), 'chat-model');
assert.equal(primaryRouteServiceId(routeTable, 'image.generate'), 'image-only');
assert.equal(primaryRouteModel(routeTable, 'image.generate'), '');
assert.equal(primaryRouteServiceId(routeTable, 'video.generate'), '');
assert.equal(primaryRouteServiceId(undefined, 'chat'), '');

const explicit = resolveMediaRoute([imageOnly, videoOnly], 'image.generate', 'image-only', '');
assert.equal(explicit.state, 'explicit');
assert.equal(explicit.service?.id, 'image-only');

const active = resolveMediaRoute([imageOnly, videoOnly], 'video.generate', '', 'video-only');
assert.equal(active.state, 'active');
assert.equal(active.service?.id, 'video-only');

const unique = resolveMediaRoute([imageOnly, videoOnly], 'image.generate', '', 'chat-only');
assert.equal(unique.state, 'unique');
assert.equal(unique.service?.id, 'image-only');

const ambiguous = resolveMediaRoute([imageOnly, service('image-two', '', {
  'image.generate': { adapter: 'ark-images', model: 'seedream-test' },
})], 'image.generate', '', '');
assert.equal(ambiguous.state, 'ambiguous');
assert.equal(ambiguous.service, null);

const missing = resolveMediaRoute([chatOnly], 'audio.speech', '', 'chat-only');
assert.equal(missing.state, 'missing');

const openai = {
  id: 'openai',
  name: 'OpenAI API',
  category: '按量付费',
  description: '',
  base_url: 'https://api.openai.com/v1',
  protocol: 'openai-responses' as const,
  default_model: 'gpt-4.1',
  models: ['gpt-4.1', 'gpt-image-1', 'tts-1'],
};
assert.deepEqual(inferPresetCapabilities(openai), ['chat', 'image.generate', 'image.edit', 'audio.speech']);
assert.deepEqual(inferPresetCapabilities({ ...openai, id: 'wan', default_model: '', models: ['wan2.6-i2v-flash'] }), ['video.generate']);
assert.deepEqual(inferPresetCapabilities({ ...openai, id: 'seedance', default_model: '', models: ['doubao-seedance-2-0-mini'] }), ['video.generate']);
assert.equal(suggestedServiceId('Fish Audio'), 'fish-audio');
assert.match(suggestedServiceId('语音服务'), /^service-[a-z0-9]+$/);

// 工具列表的顺序只由身份（内置优先 + 展示名）决定，探测/同步/修复改变状态后
// 整张表不能重排——用户会以为列表是乱序的。
function target(id: string, name: string, state: string): McpTargetDescriptor {
  return { id, name, state, detected: true, supports_auto_configure: false } as unknown as McpTargetDescriptor;
}

const beforeRepair = buildToolConnections({
  targets: [target('zeta', 'Zeta', 'configured'), target('alpha', 'Alpha', 'configured')],
  clients: [],
  acp: null,
});
assert.deepEqual(beforeRepair.map(item => item.key), ['alpha', 'zeta']);

const afterRepair = buildToolConnections({
  targets: [target('zeta', 'Zeta', 'needs_repair'), target('alpha', 'Alpha', 'configured')],
  clients: [],
  acp: null,
});
assert.deepEqual(afterRepair.map(item => item.key), ['alpha', 'zeta'], '状态变化不得改变工具列表顺序');

const withBuiltin = buildToolConnections({
  targets: [target('zeta', 'Zeta', 'configured'), target('himind-ai', 'Zzz', 'configured')],
  clients: [],
  acp: null,
});
assert.equal(withBuiltin[0].key, 'himind-ai', '内置工具必须永远排第一');

console.log('AI service routing checks passed');
