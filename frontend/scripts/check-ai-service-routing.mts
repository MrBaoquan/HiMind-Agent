import assert from 'node:assert/strict';
import { AI_CONNECTIONS_TAB_LABELS, AI_CONNECTIONS_TAB_ORDER } from '../src/pages/aiConnectionsTabs.ts';
import {
  inferPresetCapabilities,
  resolveMediaRoute,
  serviceCapabilities,
  suggestedServiceId,
} from '../src/pages/aiServiceCapabilities.ts';
import type { CustomAIService } from '../src/services/agentApi.ts';

function service(id: string, model = '', media: Record<string, { adapter: string; model: string }> = {}): CustomAIService {
  return {
    id,
    display_name: id,
    base_url: `https://${id}.example/v1`,
    protocol: 'openai-responses',
    model,
    models: model ? [model] : [],
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
assert.deepEqual(serviceCapabilities(chatOnly), ['chat']);

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

console.log('AI service routing checks passed');
