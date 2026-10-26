import type { AiServicePreset } from './aiServicePresets';
import type { CustomAIService } from '../services/agentApi';

export type AIServiceCapability =
  | 'chat'
  | 'image.generate'
  | 'image.edit'
  | 'video.generate'
  | 'audio.speech'
  | 'audio.transcribe';

export type RouteResolutionState = 'explicit' | 'active' | 'unique' | 'ambiguous' | 'missing';

export type RouteResolution = {
  state: RouteResolutionState;
  service: CustomAIService | null;
  candidates: CustomAIService[];
};

export const AI_SERVICE_CAPABILITIES: AIServiceCapability[] = [
  'chat',
  'image.generate',
  'image.edit',
  'video.generate',
  'audio.speech',
  'audio.transcribe',
];

export const AI_SERVICE_CAPABILITY_LABELS: Record<AIServiceCapability, string> = {
  chat: '对话',
  'image.generate': '生图',
  'image.edit': '改图',
  'video.generate': '视频',
  'audio.speech': '语音合成',
  'audio.transcribe': '语音转写',
};

export const MEDIA_CAPABILITIES: Array<Exclude<AIServiceCapability, 'chat'>> = [
  'image.generate',
  'image.edit',
  'video.generate',
  'audio.speech',
  'audio.transcribe',
];

export function capabilityModel(service: CustomAIService, capability: AIServiceCapability): string {
  if (capability === 'chat') {
    const model = service.model?.trim() ?? '';
    if (!model) return '';
    const mediaModels = Object.values(service.media ?? {})
      .map(binding => binding.model?.trim())
      .filter(Boolean);
    const hasAnotherChatModel = (service.models ?? [])
      .map(candidate => candidate.trim())
      .some(candidate => candidate && candidate !== model && !mediaModels.includes(candidate));
    // 旧数据曾把媒体模型写进全局 model；只有一个同名媒体模型时按媒体专用处理。
    return mediaModels.includes(model) && !hasAnotherChatModel ? '' : model;
  }
  if (capability === 'image.generate') {
    return service.media?.[capability]?.model?.trim() || service.image_model?.trim() || '';
  }
  return service.media?.[capability]?.model?.trim() ?? '';
}

export function capabilityAdapter(service: CustomAIService, capability: AIServiceCapability): string {
  if (capability === 'image.generate') {
    return service.media?.[capability]?.adapter?.trim() || service.image_adapter?.trim() || 'openai-images';
  }
  return service.media?.[capability]?.adapter?.trim() || defaultAdapterForCapability(capability);
}

export function serviceSupportsCapability(service: CustomAIService, capability: AIServiceCapability): boolean {
  return Boolean(capabilityModel(service, capability));
}

export function serviceCapabilities(service: CustomAIService): AIServiceCapability[] {
  return AI_SERVICE_CAPABILITIES.filter(capability => serviceSupportsCapability(service, capability));
}

export function defaultAdapterForCapability(capability: AIServiceCapability): string {
  switch (capability) {
    case 'image.generate': return 'openai-images';
    case 'image.edit': return 'openai-images-edit';
    case 'video.generate': return 'ark-video';
    case 'audio.speech': return 'openai-speech';
    case 'audio.transcribe': return 'openai-audio';
    default: return '';
  }
}

export function resolveMediaRoute(
  services: CustomAIService[],
  capability: Exclude<AIServiceCapability, 'chat'>,
  defaultServiceId: string,
  activeServiceId: string,
): RouteResolution {
  const candidates = services.filter(service => serviceSupportsCapability(service, capability));
  const explicit = candidates.find(service => service.id === defaultServiceId);
  if (explicit) return { state: 'explicit', service: explicit, candidates };
  const active = candidates.find(service => service.id === activeServiceId);
  if (active) return { state: 'active', service: active, candidates };
  if (candidates.length === 1) return { state: 'unique', service: candidates[0], candidates };
  if (candidates.length > 1) return { state: 'ambiguous', service: null, candidates };
  return { state: 'missing', service: null, candidates: [] };
}

export function inferPresetCapabilities(preset: AiServicePreset): string[] {
  const capabilities: string[] = [];
  if (preset.default_model.trim()) capabilities.push('chat');
  const models = preset.models.map(model => model.toLowerCase());
  if (models.some(model => model === 'gpt-image-1' || model === 'dall-e-3')) {
    capabilities.push('image.generate', 'image.edit');
  }
  if (models.some(model => model === 'tts-1')) capabilities.push('audio.speech');
  if (models.some(model => model.includes('whisper'))) capabilities.push('audio.transcribe');
  if (models.some(model => /(^|[-_/])(wan\d|kling|seedance|veo\d)/i.test(model))) capabilities.push('video.generate');
  return capabilities;
}

export function suggestedServiceId(displayName: string): string {
  const ascii = displayName
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 48);
  if (ascii.length >= 2) return ascii;
  let hash = 2166136261;
  for (const char of displayName.trim() || 'service') {
    hash ^= char.codePointAt(0) ?? 0;
    hash = Math.imul(hash, 16777619);
  }
  return `service-${(hash >>> 0).toString(36)}`;
}

