import type { AiServicePreset } from './aiServicePresets';
import type { CustomAIService, CapabilityRouteEntry } from '../services/agentApi';

export type AIServiceCapability =
  | 'chat'
  | 'image.generate'
  | 'image.edit'
  | 'video.generate'
  | 'audio.speech'
  | 'audio.transcribe'
  | 'vision.describe';

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
  'vision.describe',
];

export const AI_SERVICE_CAPABILITY_LABELS: Record<AIServiceCapability, string> = {
  chat: '对话',
  'image.generate': '生图',
  'image.edit': '改图',
  'video.generate': '视频',
  'audio.speech': '语音合成',
  'audio.transcribe': '语音转写',
  'vision.describe': '视觉理解',
};

export const MEDIA_CAPABILITIES: Array<Exclude<AIServiceCapability, 'chat'>> = [
  'image.generate',
  'image.edit',
  'video.generate',
  'audio.speech',
  'audio.transcribe',
  'vision.describe',
];

/** `service.model` 是否其实是一个媒体模型（旧数据把生图/视频模型写进了全局 model），
 *  且没有其它真正的对话模型。这类服务不能被当作对话候选。 */
function chatModelIsMediaOnly(service: CustomAIService): boolean {
  const primary = service.model?.trim() ?? '';
  if (!primary) return false;
  const mediaModels = Object.values(service.media ?? {})
    .map(binding => binding.model?.trim())
    .filter(Boolean);
  if (!mediaModels.includes(primary)) return false;
  return !(service.models ?? []).some(model => {
    const candidate = model.trim();
    return candidate && candidate !== primary && !mediaModels.includes(candidate);
  });
}

/** 某服务在某能力下的模型目录：默认模型在前，其余按声明顺序去重。
 *  必须与后端 `capability_models` 保持一致，否则界面会给出路由实际不会用的模型。 */
export function capabilityModels(service: CustomAIService, capability: AIServiceCapability): string[] {
  const models: string[] = [];
  const push = (model: string | undefined | null) => {
    const value = model?.trim() ?? '';
    if (value && !models.includes(value)) models.push(value);
  };
  if (capability === 'chat') {
    if (chatModelIsMediaOnly(service)) return models;
    push(service.model);
    for (const model of service.models ?? []) push(model);
    return models;
  }
  const binding = service.media?.[capability];
  if (binding) {
    push(binding.model);
    for (const model of binding.models ?? []) push(model);
    return models;
  }
  if (capability === 'vision.describe') {
    // 没有显式 vision 绑定时跟随对话模型（ADR 0023 第七条）。
    if (chatModelIsMediaOnly(service)) return models;
    push(service.model);
    for (const model of service.models ?? []) push(model);
    return models;
  }
  if (capability === 'image.generate') push(service.image_model);
  return models;
}

/** 该能力的默认模型，即路由项 `model` 为空时真正会用的模型。 */
export function capabilityModel(service: CustomAIService, capability: AIServiceCapability): string {
  return capabilityModels(service, capability)[0] ?? '';
}

/** 某能力路由表的主选服务 ID。下标 0 是主选，其余是备用（ADR 0023）。
 *  各面板「默认」标记必须走这里，避免各自内联一套取值规则。 */
export function primaryRouteServiceId(
  routes: Record<string, CapabilityRouteEntry[]> | undefined,
  capability: AIServiceCapability,
): string {
  return routes?.[capability]?.[0]?.service_id ?? '';
}

/** 某能力路由表主选上钉的模型；空串表示跟随该服务的默认模型。 */
export function primaryRouteModel(
  routes: Record<string, CapabilityRouteEntry[]> | undefined,
  capability: AIServiceCapability,
): string {
  return routes?.[capability]?.[0]?.model ?? '';
}

export function capabilityAdapter(service: CustomAIService, capability: AIServiceCapability): string {
  if (capability === 'image.generate') {
    return service.media?.[capability]?.adapter?.trim() || service.image_adapter?.trim() || 'openai-images';
  }
  return service.media?.[capability]?.adapter?.trim() || defaultAdapterForCapability(capability);
}

export function serviceSupportsCapability(service: CustomAIService, capability: AIServiceCapability): boolean {
  // 对话只要求有对话模型；`chat` 的候选集不因旧数据里把媒体模型塞进 `model` 而缩小。
  return capabilityModels(service, capability).length > 0;
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
    case 'vision.describe': return '';
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

