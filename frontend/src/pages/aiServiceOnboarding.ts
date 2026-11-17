import type { CustomAIService } from '../services/agentApi';
import type { AiServicePreset } from './aiServicePresets';
import { AI_SERVICE_CAPABILITIES, capabilityModels, defaultAdapterForCapability, type AIServiceCapability as Capability } from './aiServiceCapabilities.ts';

export type ModelEntry = {
  id: string;
  source: 'preset' | 'discovered' | 'saved' | 'manual' | 'retained';
  selected: boolean;
  operations: Capability[];
  suggestions: Capability[];
};
export type ModelSetup = {
  entries: ModelEntry[];
  defaults: Partial<Record<Capability, string>>;
  adapters: Partial<Record<Capability, string>>;
};
export const emptyModelSetup = (): ModelSetup => ({ entries: [], defaults: {}, adapters: {} });
export const modelSourceLabels = { preset: '预设目录', discovered: '服务目录', saved: '已保存配置', manual: '手动添加', retained: '先前选择' };

// Suggestions are explicit template metadata, never inferred from a model-name pattern.
function presetOperations(preset: AiServicePreset, id: string): Capability[] {
  if (preset.id === 'wan-video') return ['video.generate'];
  if (preset.id === 'openai') {
    const known: Record<string, Capability[]> = {
      'gpt-image-1': ['image.generate', 'image.edit'], 'dall-e-3': ['image.generate'],
      'tts-1': ['audio.speech'], 'whisper-1': ['audio.transcribe'],
      'gpt-4.1': ['chat', 'vision.describe'], 'gpt-4.1-mini': ['chat', 'vision.describe'],
      'gpt-4o': ['chat', 'vision.describe'], o3: ['chat', 'vision.describe'],
    };
    return known[id] ?? (id === preset.default_model ? ['chat'] : []);
  }
  return id === preset.default_model ? ['chat'] : [];
}

export function modelsFromPreset(preset: AiServicePreset): ModelSetup {
  const entries = [...new Set([preset.default_model, ...preset.models].filter(Boolean))].map(id => ({
    id, source: 'preset' as const, selected: id === (preset.default_model || preset.models[0]),
    operations: presetOperations(preset, id), suggestions: presetOperations(preset, id),
  }));
  // Unselected suggestions aren't configuration until the user adds that model.
  return { entries, defaults: {}, adapters: preset.id === 'wan-video' ? { 'video.generate': 'wan-video' } : {} };
}

export function modelsFromService(service: CustomAIService): ModelSetup {
  const setup = emptyModelSetup();
  for (const operation of AI_SERVICE_CAPABILITIES) {
    const models = capabilityModels(service, operation);
    if (!models.length) continue;
    setup.defaults[operation] = models[0];
    if (service.media?.[operation]?.adapter) setup.adapters[operation] = service.media[operation].adapter;
    for (const id of models) {
      const existing = setup.entries.find(entry => entry.id === id);
      if (existing) existing.operations.push(operation);
      else setup.entries.push({ id, source: 'saved', selected: true, operations: [operation], suggestions: [] });
    }
  }
  return setup;
}

export function mergeDiscoveredModels(setup: ModelSetup, ids: string[]): ModelSetup {
  const discovered = [...new Set(ids.map(id => id.trim()).filter(Boolean))];
  return { ...setup, entries: [
    ...discovered.map(id => {
      const existing = setup.entries.find(entry => entry.id === id);
      return existing ? { ...existing, source: 'discovered' as const }
        : { id, source: 'discovered' as const, selected: false, operations: [], suggestions: [] };
    }),
    ...setup.entries.filter(entry => !discovered.includes(entry.id) && (entry.selected || entry.source === 'manual')),
  ] };
}

export function configuredOperations(setup: ModelSetup): Capability[] {
  return AI_SERVICE_CAPABILITIES.filter(op => setup.entries.some(entry => entry.selected && entry.operations.includes(op)));
}

export function modelSetupError(setup: ModelSetup): string {
  const selected = setup.entries.filter(entry => entry.selected);
  if (!selected.length) return '请选择至少一个模型；没有模型目录时可手动添加。';
  const unknown = selected.find(entry => !entry.operations.length);
  if (unknown) return `请为模型「${unknown.id}」选择用途；目录未提供能力信息。`;
  return '';
}

export function modelConfiguration(setup: ModelSetup) {
  const error = modelSetupError(setup);
  if (error) throw new Error(error);
  const available = (op: Capability) => setup.entries.filter(entry => entry.selected && entry.operations.includes(op)).map(entry => entry.id);
  const primary = (op: Capability) => {
    const models = available(op);
    return models.includes(setup.defaults[op] ?? '') ? setup.defaults[op]! : models[0] ?? '';
  };
  const adapter = (op: Capability) => available(op).length ? setup.adapters[op] || defaultAdapterForCapability(op) : '';
  const media_models = Object.fromEntries(configuredOperations(setup).filter(op => op !== 'chat')
    .map(op => [op, available(op).filter(id => id !== primary(op))]));
  return {
    enabled_capabilities: configuredOperations(setup),
    model: primary('chat'), models: available('chat'), media_models,
    image_model: primary('image.generate'), image_adapter: adapter('image.generate'),
    image_edit_model: primary('image.edit'), image_edit_adapter: adapter('image.edit'),
    video_model: primary('video.generate'), video_adapter: adapter('video.generate'),
    speech_model: primary('audio.speech'), speech_adapter: adapter('audio.speech'),
    transcribe_model: primary('audio.transcribe'), transcribe_adapter: adapter('audio.transcribe'),
    vision_model: primary('vision.describe'),
  };
}
