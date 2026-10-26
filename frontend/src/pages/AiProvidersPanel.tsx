import { useEffect, useRef, useState } from 'react';
import { Check, ChevronDown, ChevronLeft, ChevronRight, CircleDashed, CircleX, ExternalLink, Pencil, PlugZap, RefreshCw, Search, ShieldCheck, Sparkles, Trash2, X } from 'lucide-react';
import { Pill } from '../components/Common';
import { BusyIndicator } from '../components/BusyIndicator';
import { useConfirm } from '../components/ConfirmDialog';
import { fallbackAiServicePresets, type AiServicePreset } from './aiServicePresets';
import { inferBalanceProfile } from './balanceProfileMatching';
import { clientLabel } from '../utils/clientLabels';
import { agentApi } from '../services/agentApi';
import type { AIProviderImportStatus, AIServiceBalanceSnapshot, AIServiceListResult, AIServiceProtocol, AIServiceTemplateListResult, CustomAIService, ManagedAIServiceSummary, MediaProviderCatalogResult } from '../services/agentApi';
import {
  AI_SERVICE_CAPABILITIES,
  AI_SERVICE_CAPABILITY_LABELS,
  MEDIA_CAPABILITIES,
  capabilityAdapter,
  capabilityModel,
  inferPresetCapabilities,
  serviceCapabilities,
  serviceSupportsCapability,
  suggestedServiceId,
  type AIServiceCapability,
} from './aiServiceCapabilities';

const protocolOptions: Array<{ value: AIServiceProtocol; label: string }> = [
  { value: 'openai-responses', label: 'OpenAI Responses' },
  { value: 'openai-chat', label: 'OpenAI Chat' },
  { value: 'anthropic', label: 'Anthropic Messages' },
];

const protocolLabels: Record<AIServiceProtocol, string> = {
  'openai-responses': 'Responses',
  'openai-chat': 'Chat',
  anthropic: 'Anthropic',
};

const mediaCapabilityLabels: Record<string, string> = {
  'image.generate': '生图',
  'image.edit': '改图',
  'video.generate': '生视频',
  'audio.speech': '语音合成',
  'audio.transcribe': '语音转写',
};

const executionModeLabels: Record<string, string> = {
  sync: '同步',
  stream: '流式',
  async: '异步',
};

const builtInMediaPresets: AiServicePreset[] = [
  {
    id: 'wan-video',
    name: '百炼万相视频',
    category: '媒体生成',
    description: '阿里云百炼万相文生视频 / 图生视频',
    base_url: 'https://dashscope.aliyuncs.com',
    protocol: 'openai-responses',
    default_model: '',
    models: ['wan2.6-i2v-flash', 'wan2.6-i2v', 'wan2.6-t2v', 'wan2.5-i2v-preview'],
  },
];

const balanceProfileOptions = [
  { value: 'deepseek', label: 'DeepSeek 官方账户余额' },
  { value: 'kimi', label: 'Kimi / Moonshot 官方账户余额' },
  { value: 'kimi-coding', label: 'Kimi Coding Plan 套餐配额' },
  { value: 'zhipu-plan', label: '智谱 GLM Coding Plan 配额' },
  { value: 'minimax-plan-cn', label: 'MiniMax Coding Plan 国内版' },
  { value: 'minimax-plan-en', label: 'MiniMax Coding Plan 国际版' },
  { value: 'stepfun', label: 'StepFun 官方账户余额' },
  { value: 'siliconflow', label: 'SiliconFlow 国内账户余额' },
  { value: 'siliconflow-en', label: 'SiliconFlow 国际站余额' },
  { value: 'openrouter', label: 'OpenRouter 账户余额' },
  { value: 'novita', label: 'Novita AI 账户余额' },
  { value: 'relay-balance', label: '兼容中转 · /user/balance' },
  { value: 'new-api', label: 'New API · 用户额度' },
  { value: 'sub2api', label: 'Sub2API · 用量余额' },
  { value: 'opencode-go', label: 'OpenCode Go · 套餐窗口' },
] as const;

function balanceProfileForPreset(baseUrl: string): string {
  return inferBalanceProfile(baseUrl) ?? '';
}


type AiProvidersPanelProps = {
  aiServices: AIServiceListResult | null;
  templates: AIServiceTemplateListResult | null;
  onRefresh: () => void;
  onSaveAIService: (input: {
    id: string;
    display_name: string;
    base_url: string;
    protocol: AIServiceProtocol;
    model: string;
    models: string[];
    image_model: string;
    image_adapter: string;
    image_edit_model: string;
    image_edit_adapter: string;
    video_model: string;
    video_adapter: string;
    speech_model: string;
    speech_adapter: string;
    transcribe_model: string;
    transcribe_adapter: string;
    balance_profile_id: string;
    balance_access_token: string;
    balance_user_id: string;
    api_key: string;
  }) => Promise<void>;
  onRemoveAIService: (id: string) => Promise<void>;
  onOpenAccount: () => void;
  onFetchModels: (input: { base_url: string; api_key: string; protocol: AIServiceProtocol }) => Promise<string[]>;
  onFetchSavedModels: (id: string, base_url: string) => Promise<string[]>;
  onFetchBalance: (id: string) => Promise<AIServiceBalanceSnapshot>;
};

type FormStep = 0 | 1 | 2;
type ConnectionCheck = { state: 'busy' | 'ok' | 'error'; message: string };

const emptyDraft = {
  id: '',
  display_name: '',
  base_url: '',
  protocol: 'openai-responses' as AIServiceProtocol,
  model: '',
  models: '',
  image_model: '',
  image_adapter: '',
  image_edit_model: '',
  image_edit_adapter: '',
  video_model: '',
  video_adapter: '',
  speech_model: '',
  speech_adapter: '',
  transcribe_model: '',
  transcribe_adapter: '',
  balance_profile_id: '',
  balance_access_token: '',
  balance_user_id: '',
  api_key: '',
};

function serviceHealthLabel(provider: MediaProviderCatalogResult['providers'][number] | undefined, hasApiKey: boolean): { text: string; kind: 'success' | 'warn' | 'neutral' } {
  if (!hasApiKey || provider?.health.state === 'unconfigured') return { text: '缺少凭据', kind: 'warn' };
  if (provider?.health.state === 'unavailable' || provider?.health.state === 'degraded') return { text: '连接异常', kind: 'warn' };
  return { text: '凭据已配置', kind: 'success' };
}

export function AiProvidersPanel({
  aiServices,
  templates,
  onRefresh,
  onSaveAIService,
  onRemoveAIService,
  onOpenAccount,
  onFetchModels,
  onFetchSavedModels,
  onFetchBalance,
}: AiProvidersPanelProps) {
  const [formOpen, setFormOpen] = useState(false);
  const [formStep, setFormStep] = useState<FormStep>(0);
  const [editingServiceId, setEditingServiceId] = useState<string | null>(null);
  const [draft, setDraft] = useState(emptyDraft);
  const [enabledCapabilities, setEnabledCapabilities] = useState<AIServiceCapability[]>(['chat']);
  const [saving, setSaving] = useState(false);
  const [fetchingModels, setFetchingModels] = useState(false);
  const [formError, setFormError] = useState('');
  const [balanceSnapshots, setBalanceSnapshots] = useState<Record<string, AIServiceBalanceSnapshot>>(aiServices?.custom?.balances ?? {});
  const [providerCatalog, setProviderCatalog] = useState<MediaProviderCatalogResult | null>(null);
  const [balanceRefreshingId, setBalanceRefreshingId] = useState<string | null>(null);
  const [balanceErrors, setBalanceErrors] = useState<Record<string, string>>({});
  const [connectionChecks, setConnectionChecks] = useState<Record<string, ConnectionCheck>>({});
  const [searchQuery, setSearchQuery] = useState('');
  const [capabilityFilter, setCapabilityFilter] = useState<'all' | AIServiceCapability>('all');
  const [selectedPreset, setSelectedPreset] = useState<string>('');
  const [presetCategory, setPresetCategory] = useState<string>('');
  const modalRef = useRef<HTMLElement | null>(null);
  const restoreFocusRef = useRef<HTMLElement | null>(null);

  const customServices = aiServices?.custom?.services ?? [];
  const activeServiceId = aiServices?.custom?.active_service_id ?? '';
  const mediaDefaults = aiServices?.custom?.media_services ?? {};
  const imageServiceId = aiServices?.custom?.image_service_id ?? '';
  const managed = aiServices?.managed ?? { available: false };
  const clientStatuses = aiServices?.clients?.targets ?? [];
  const serviceCount = customServices.length;
  const editing = editingServiceId !== null;

  useEffect(() => {
    setBalanceSnapshots(aiServices?.custom?.balances ?? {});
  }, [aiServices?.custom?.balances]);

  useEffect(() => {
    let canceled = false;
    void agentApi.getMediaProviderCatalog()
      .then((catalog) => { if (!canceled) setProviderCatalog(catalog); })
      .catch(() => { if (!canceled) setProviderCatalog(null); });
    return () => { canceled = true; };
  }, [aiServices]);

  useEffect(() => {
    if (!formOpen) {
      restoreFocusRef.current?.focus();
      restoreFocusRef.current = null;
      return;
    }
    const modal = modalRef.current;
    if (!modal) return;
    const focusable = () => Array.from(modal.querySelectorAll<HTMLElement>(
      'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
    )).filter((element) => element.offsetParent !== null);
    window.setTimeout(() => focusable()[0]?.focus(), 0);
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && !saving) {
        setFormOpen(false);
        setEditingServiceId(null);
        return;
      }
      if (event.key !== 'Tab') return;
      const elements = focusable();
      if (!elements.length) return;
      const first = elements[0];
      const last = elements[elements.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [formOpen, saving]);

  function serviceHealth(service: CustomAIService) {
    const provider = providerCatalog?.providers.find(item => item.id === service.id);
    return serviceHealthLabel(provider, Boolean(provider?.has_api_key ?? service.models.length));
  }

  function routeBadges(service: CustomAIService) {
    const labels: string[] = [];
    if (activeServiceId === service.id) labels.push('对话默认');
    for (const capability of MEDIA_CAPABILITIES) {
      const selected = mediaDefaults[capability] || (capability === 'image.generate' ? imageServiceId : '');
      if (selected === service.id) labels.push(`${AI_SERVICE_CAPABILITY_LABELS[capability]}默认`);
    }
    return labels;
  }

  function boundClients(service: CustomAIService) {
    return clientStatuses.filter(client => client.state === 'imported' && client.service === `custom:${service.id}`);
  }

  const filteredServices = customServices.filter(service => {
    if (capabilityFilter !== 'all' && !serviceSupportsCapability(service, capabilityFilter)) return false;
    const query = searchQuery.trim().toLowerCase();
    if (!query) return true;
    const haystack = [
      service.display_name,
      service.id,
      service.base_url,
      service.protocol,
      service.model,
      ...serviceCapabilities(service).map(capability => AI_SERVICE_CAPABILITY_LABELS[capability]),
    ].join(' ').toLowerCase();
    return haystack.includes(query);
  });
  const workbenchPresets = templates?.items ?? [];
  const usingFallbackPresets = workbenchPresets.length === 0;
  const presetEntries: AiServicePreset[] = usingFallbackPresets
    ? [...builtInMediaPresets, ...fallbackAiServicePresets.filter(preset => !builtInMediaPresets.some(item => item.id === preset.id))]
    : workbenchPresets;

  function applyPreset(presetId: string) {
    const preset = presetEntries.find((item) => item.id === presetId);
    if (!preset) return;
    const inferred = inferPresetCapabilities(preset) as AIServiceCapability[];
    const isWanVideo = preset.id === 'wan-video';
    const enabled: AIServiceCapability[] = isWanVideo ? ['video.generate'] : inferred;
    const videoModel = isWanVideo ? preset.models[0] ?? '' : '';
    const imageModel = preset.models.find(model => model === 'gpt-image-1' || model === 'dall-e-3') ?? '';
    const editModel = preset.models.find(model => model === 'gpt-image-1') ?? '';
    const speechModel = preset.models.find(model => model === 'tts-1') ?? '';
    const transcribeModel = preset.models.find(model => model.toLowerCase().includes('whisper')) ?? '';
    setSelectedPreset(presetId);
    setEnabledCapabilities(enabled);
    setFormStep(0);
    setFormError('');
    setDraft({
      id: preset.id,
      display_name: preset.name,
      base_url: preset.base_url,
      protocol: preset.protocol,
      model: preset.default_model,
      models: preset.models.join(', '),
      image_model: imageModel,
      image_adapter: imageModel ? 'openai-images' : '',
      image_edit_model: editModel,
      image_edit_adapter: editModel ? 'openai-images-edit' : '',
      video_model: videoModel,
      video_adapter: isWanVideo ? 'wan-video' : '',
      speech_model: speechModel,
      speech_adapter: speechModel ? 'openai-speech' : '',
      transcribe_model: transcribeModel,
      transcribe_adapter: transcribeModel ? 'openai-audio' : '',
      balance_profile_id: balanceProfileForPreset(preset.base_url),
      balance_access_token: '',
      balance_user_id: '',
      api_key: '',
    });
  }

  const presetGroups = (() => {
    const groups = new Map<string, AiServicePreset[]>();
    for (const preset of presetEntries) {
      const name = preset.category.trim() || '其他';
      const items = groups.get(name);
      if (items) items.push(preset);
      else groups.set(name, [preset]);
    }
    return [...groups].map(([name, items]) => ({ name, items }));
  })();
  const activePresetGroup = presetGroups.find((group) => group.name === presetCategory);

  function draftModelFor(capability: AIServiceCapability) {
    switch (capability) {
      case 'chat': return draft.model.trim();
      case 'image.generate': return draft.image_model.trim();
      case 'image.edit': return draft.image_edit_model.trim();
      case 'video.generate': return draft.video_model.trim();
      case 'audio.speech': return draft.speech_model.trim();
      case 'audio.transcribe': return draft.transcribe_model.trim();
    }
  }

  function toggleCapability(capability: AIServiceCapability) {
    setFormError('');
    setEnabledCapabilities(current => {
      if (current.includes(capability)) return current.filter(item => item !== capability);
      return [...current, capability];
    });
    setDraft(current => {
      if (capability === 'image.generate' && !current.image_adapter) return { ...current, image_adapter: 'openai-images' };
      if (capability === 'image.edit' && !current.image_edit_adapter) return { ...current, image_edit_adapter: 'openai-images-edit' };
      if (capability === 'video.generate' && !current.video_adapter) return { ...current, video_adapter: 'ark-video' };
      if (capability === 'audio.speech' && !current.speech_adapter) return { ...current, speech_adapter: 'openai-speech' };
      if (capability === 'audio.transcribe' && !current.transcribe_adapter) return { ...current, transcribe_adapter: 'openai-audio' };
      return current;
    });
  }

  function connectionStepError() {
    if (!draft.display_name.trim()) return '请先填写「服务名称」。';
    if (!editing && !draft.api_key.trim()) return '请先填写「API Key」。';
    if (!draft.base_url.trim()) return '请先填写「Base URL」。';
    return '';
  }

  function capabilityStepError() {
    if (!enabledCapabilities.length) return '请至少启用一项能力。';
    const missing = enabledCapabilities.find(capability => !draftModelFor(capability));
    if (missing) return `请为「${AI_SERVICE_CAPABILITY_LABELS[missing]}」填写模型。`;
    return '';
  }

  function goNext() {
    const error = formStep === 0 ? connectionStepError() : capabilityStepError();
    if (error) {
      setFormError(error);
      return;
    }
    setFormError('');
    setFormStep(current => Math.min(2, current + 1) as FormStep);
  }

  function goBack() {
    setFormError('');
    setFormStep(current => Math.max(0, current - 1) as FormStep);
  }

  async function saveService() {
    const error = connectionStepError() || capabilityStepError();
    if (error) {
      setFormError(error);
      return;
    }
    if (draft.balance_profile_id === 'new-api' && !draft.balance_user_id.trim()) {
      setFormError('New API 余额查询需要填写 User ID。');
      return;
    }
    const enabled = new Set(enabledCapabilities);
    const id = draft.id.trim() || suggestedServiceId(draft.display_name);
    setFormError('');
    setSaving(true);
    try {
      await onSaveAIService({
        id,
        display_name: draft.display_name.trim(),
        base_url: draft.base_url.trim(),
        protocol: draft.protocol,
        model: enabled.has('chat') ? draft.model.trim() : '',
        models: enabled.has('chat') ? draft.models.split(/[\n,]/).map((item) => item.trim()).filter(Boolean) : [],
        image_model: enabled.has('image.generate') ? draft.image_model.trim() : '',
        image_adapter: enabled.has('image.generate') ? (draft.image_adapter || 'openai-images') : '',
        image_edit_model: enabled.has('image.edit') ? draft.image_edit_model.trim() : '',
        image_edit_adapter: enabled.has('image.edit') ? (draft.image_edit_adapter || 'openai-images-edit') : '',
        video_model: enabled.has('video.generate') ? draft.video_model.trim() : '',
        video_adapter: enabled.has('video.generate') ? (draft.video_adapter || 'ark-video') : '',
        speech_model: enabled.has('audio.speech') ? draft.speech_model.trim() : '',
        speech_adapter: enabled.has('audio.speech') ? (draft.speech_adapter || 'openai-speech') : '',
        transcribe_model: enabled.has('audio.transcribe') ? draft.transcribe_model.trim() : '',
        transcribe_adapter: enabled.has('audio.transcribe') ? (draft.transcribe_adapter || 'openai-audio') : '',
        balance_profile_id: draft.balance_profile_id,
        balance_access_token: draft.balance_access_token.trim(),
        balance_user_id: draft.balance_user_id.trim(),
        api_key: draft.api_key.trim(),
      });
      setDraft(emptyDraft);
      setEnabledCapabilities(['chat']);
      setFormStep(0);
      setEditingServiceId(null);
      setSelectedPreset('');
      setPresetCategory('');
      setFormOpen(false);
    } catch (error) {
      setFormError(formatAIServiceError(error, '保存服务失败，请检查连接信息后重试。'));
    } finally {
      setSaving(false);
    }
  }

  async function refreshBalance(serviceId: string) {
    if (balanceRefreshingId) return;
    setBalanceRefreshingId(serviceId);
    setBalanceErrors((current) => ({ ...current, [serviceId]: '' }));
    try {
      const snapshot = await onFetchBalance(serviceId);
      setBalanceSnapshots((current) => ({ ...current, [serviceId]: snapshot }));
    } catch (error) {
      setBalanceErrors((current) => ({
        ...current,
        [serviceId]: formatAIServiceError(error, '余额查询失败，请稍后重试。'),
      }));
    } finally {
      setBalanceRefreshingId(null);
    }
  }

  async function fetchModels() {
    if (!draft.base_url.trim() || (!draft.api_key.trim() && !editing)) return;
    setFormError('');
    setFetchingModels(true);
    try {
      const models = draft.api_key.trim()
        ? await onFetchModels({ base_url: draft.base_url.trim(), api_key: draft.api_key.trim(), protocol: draft.protocol })
        : await onFetchSavedModels(draft.id.trim(), draft.base_url.trim());
      setDraft((current) => ({ ...current, models: models.join(', ') }));
    } catch (error) {
      setFormError(formatAIServiceError(error, '获取模型失败，请检查 Base URL 和 API Key。'));
    } finally {
      setFetchingModels(false);
    }
  }

  async function checkServiceConnection(service: CustomAIService) {
    if (!capabilityModel(service, 'chat')) {
      setConnectionChecks(current => ({ ...current, [service.id]: { state: 'error', message: '媒体服务需在能力路由中实际调用验证' } }));
      return;
    }
    setConnectionChecks(current => ({ ...current, [service.id]: { state: 'busy', message: '检查中…' } }));
    try {
      const models = await onFetchSavedModels(service.id, service.base_url);
      setConnectionChecks(current => ({ ...current, [service.id]: { state: 'ok', message: `模型接口可用 · ${models.length} 个模型` } }));
    } catch (error) {
      setConnectionChecks(current => ({ ...current, [service.id]: { state: 'error', message: formatAIServiceError(error, '模型接口不可用') } }));
    }
  }

  function openNewService() {
    restoreFocusRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setSelectedPreset('');
    setPresetCategory('');
    setEditingServiceId(null);
    setDraft(emptyDraft);
    setEnabledCapabilities(['chat']);
    setFormStep(0);
    setFormError('');
    setFormOpen(true);
  }

  function openEditService(service: CustomAIService) {
    restoreFocusRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setSelectedPreset('');
    setPresetCategory('');
    setEditingServiceId(service.id);
    setDraft({
      id: service.id,
      display_name: service.display_name,
      base_url: service.base_url,
      protocol: service.protocol,
      model: service.model,
      models: service.models.join(', '),
      image_model: service.image_model ?? '',
      image_adapter: service.image_adapter ?? '',
      image_edit_model: service.media?.['image.edit']?.model ?? '',
      image_edit_adapter: service.media?.['image.edit']?.adapter ?? '',
      video_model: service.media?.['video.generate']?.model ?? '',
      video_adapter: service.media?.['video.generate']?.adapter ?? '',
      speech_model: service.media?.['audio.speech']?.model ?? '',
      speech_adapter: service.media?.['audio.speech']?.adapter ?? '',
      transcribe_model: service.media?.['audio.transcribe']?.model ?? '',
      transcribe_adapter: service.media?.['audio.transcribe']?.adapter ?? '',
      balance_profile_id: service.balance_profile_id ?? '',
      balance_access_token: '',
      balance_user_id: service.balance_user_id ?? '',
      api_key: '',
    });
    const capabilities = serviceCapabilities(service);
    setEnabledCapabilities(capabilities.length ? capabilities : ['chat']);
    setFormStep(0);
    setFormError('');
    setFormOpen(true);
  }

  function closeForm() {
    if (saving) return;
    setFormOpen(false);
    setEditingServiceId(null);
    setFormStep(0);
  }
  return (
    <div className="ai-providers-view">
      <section className={`ai-overview ${serviceCount ? 'ready' : 'attention'}`}>
        <div className="ai-overview-main">
          <div className="ai-overview-icon"><PlugZap size={20} /></div>
          <div className="ai-overview-copy">
            <span className="ai-overview-eyebrow">AI 服务</span>
            <strong>{serviceCount ? `${serviceCount} 个本机服务` : '还没有可用的 AI 服务'}</strong>
            <span>一个服务可承载对话或媒体能力；默认路由在「能力路由」查看。</span>
          </div>
        </div>
        <div className="ai-overview-actions">
          <button className="btn btn-primary" onClick={openNewService}>新增服务</button>
        </div>
      </section>

      <section className="ai-managed-section" aria-labelledby="ai-managed-title">
        <div className="ai-section-heading">
          <div><h3 id="ai-managed-title">工作台服务</h3><span>由 AI 工作台管理，不计入本机服务数量。</span></div>
        </div>
        <ManagedSourceRow managed={managed} active={!activeServiceId} clientStatuses={clientStatuses} onOpenAccount={onOpenAccount} onRefresh={onRefresh} />
      </section>

      <section className="ai-client-section" aria-labelledby="ai-services-title">
        <div className="ai-section-heading">
          <div><h3 id="ai-services-title">本机服务</h3><span>连接、凭据和能力在这里管理；路由与工具分发在各自页面完成。</span></div>
          <div className="ai-section-tail"><Pill kind="neutral">{serviceCount}</Pill></div>
        </div>

        <div className="ai-service-toolbar">
          <label className="ai-service-search" aria-label="搜索 AI 服务">
            <Search size={14} />
            <input value={searchQuery} onChange={event => setSearchQuery(event.target.value)} placeholder="搜索名称、地址、协议或能力" />
          </label>
          <select className="ai-service-filter" aria-label="按能力筛选 AI 服务" value={capabilityFilter} onChange={event => setCapabilityFilter(event.target.value as 'all' | AIServiceCapability)}>
            <option value="all">全部能力</option>
            {AI_SERVICE_CAPABILITIES.map(capability => <option key={capability} value={capability}>{AI_SERVICE_CAPABILITY_LABELS[capability]}</option>)}
          </select>
        </div>

        <div className="ai-client-list">
          {filteredServices.map((service) => (
            <CustomSourceRow
              key={service.id}
              service={service}
              active={activeServiceId === service.id}
              capabilities={serviceCapabilities(service)}
              routeBadges={routeBadges(service)}
              health={serviceHealth(service)}
              connectionCheck={connectionChecks[service.id]}
              clientStatuses={boundClients(service)}
              balanceSnapshot={balanceSnapshots[service.id]}
              balanceLoading={balanceRefreshingId === service.id}
              balanceBusy={balanceRefreshingId !== null}
              balanceError={balanceErrors[service.id] ?? ''}
              onCheckConnection={() => void checkServiceConnection(service)}
              onRefreshBalance={() => void refreshBalance(service.id)}
              onEdit={() => openEditService(service)}
              onRemove={() => void onRemoveAIService(service.id)}
            />
          ))}
          {!serviceCount ? <div className="ai-empty-row"><span className="ai-empty-icon"><CircleDashed size={14} /></span><span>暂无本机 AI 服务，点「新增服务」连接一个模型来源。</span></div> : null}
          {serviceCount && !filteredServices.length ? <div className="ai-empty-row"><span className="ai-empty-icon"><Search size={14} /></span><span>没有匹配的服务，换一个关键词或能力筛选。</span></div> : null}
        </div>
      </section>

      {formOpen ? (
        <div className="modal-backdrop" onMouseDown={(event) => { if (event.target === event.currentTarget && !saving) closeForm(); }}>
          <section ref={modalRef} className="modal ai-service-modal" role="dialog" aria-modal="true" aria-labelledby="ai-service-modal-title">
            <div className="modal-header">
              <div>
                <h3 id="ai-service-modal-title">{editing ? '编辑 AI 服务' : '新增 AI 服务'}</h3>
                <p>连接信息和密钥只保存在本机。</p>
              </div>
              <button className="btn btn-icon" title="关闭" aria-label="关闭" disabled={saving} onClick={closeForm}><X size={16} /></button>
            </div>
            <div className="modal-body ai-service-modal-body">
              {formError ? <div className="ai-service-form-error" role="alert"><CircleX size={15} /><span>{formError}</span></div> : null}
              <div className="ai-service-steps" aria-label="配置步骤">
                {['连接', '能力与模型', '高级与用量'].map((label, index) => (
                  <span key={label} className={`ai-service-step${formStep === index ? ' active' : ''}${formStep > index ? ' complete' : ''}`}>
                    <b>{formStep > index ? <Check size={12} /> : index + 1}</b>{label}
                  </span>
                ))}
              </div>

              {formStep === 0 ? <>
                <div className="ai-service-presets">
                  <div className="ai-service-presets-head">
                    <div className="ai-service-group-label">常用服务</div>
                    {usingFallbackPresets ? <span className="ai-service-preset-source" title="连接 AI 工作台后会改用工作台目录里的服务与模型">未连接 AI 工作台，使用内置预设</span> : null}
                  </div>
                  <div className="ai-service-preset-tabs">
                    <button type="button" className={`ai-service-preset-tab${!presetCategory ? ' active' : ''}`} onClick={() => { setPresetCategory(''); setSelectedPreset(''); setDraft(emptyDraft); setEnabledCapabilities(['chat']); }}>
                      <Pencil size={13} />手动配置
                    </button>
                    {presetGroups.map((group) => (
                      <button key={group.name} type="button" className={`ai-service-preset-tab${presetCategory === group.name ? ' active' : ''}`} onClick={() => setPresetCategory(group.name)}>
                        <Sparkles size={13} />{group.name}
                      </button>
                    ))}
                  </div>
                  {activePresetGroup ? <div className="ai-service-preset-list">
                    {activePresetGroup.items.map((preset) => (
                      <button key={preset.id} type="button" className={`ai-service-preset-chip${selectedPreset === preset.id ? ' active' : ''}`} title={preset.description} onClick={() => applyPreset(preset.id)}>
                        <strong>{preset.name}</strong><span>{preset.description}</span>
                      </button>
                    ))}
                  </div> : <div className="ai-service-preset-hint"><Sparkles size={13} />选择常用服务后会自动填入连接信息、能力和推荐模型。</div>}
                </div>

                <div className="ai-service-form-group">
                  <div className="ai-service-group-label">连接</div>
                  <div className="ai-service-form-grid">
                    <label className="field-label ai-service-field ai-service-field-wide"><span>服务名称</span><input value={draft.display_name} onChange={(event) => setDraft((current) => ({ ...current, display_name: event.target.value }))} placeholder="例如：公司模型 / Fish Audio" /></label>
                    <label className="field-label ai-service-field ai-service-field-wide"><span>API Key</span><input type="password" value={draft.api_key} onChange={(event) => setDraft((current) => ({ ...current, api_key: event.target.value }))} placeholder={editing ? '留空以保留当前 Key' : 'sk-...'} /></label>
                    <label className="field-label ai-service-field"><span>Base URL</span><input value={draft.base_url} onChange={(event) => setDraft((current) => ({ ...current, base_url: event.target.value }))} placeholder="https://api.example.com/v1" /></label>
                    <label className="field-label ai-service-field"><span>接口类型</span><select value={draft.protocol} onChange={(event) => setDraft((current) => ({ ...current, protocol: event.target.value as AIServiceProtocol }))}>{protocolOptions.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}</select></label>
                  </div>
                  <small className="ai-service-hint">主协议用于服务连接；媒体适配器在下一步选择。</small>
                </div>
              </> : null}

              {formStep === 1 ? <>
                <div className="ai-service-form-group">
                  <div className="ai-service-group-label">启用能力</div>
                  <div className="ai-capability-picker">
                    {AI_SERVICE_CAPABILITIES.map(capability => (
                      <label key={capability} className={`ai-capability-option${enabledCapabilities.includes(capability) ? ' selected' : ''}`}>
                        <input type="checkbox" checked={enabledCapabilities.includes(capability)} onChange={() => toggleCapability(capability)} />
                        <span><strong>{AI_SERVICE_CAPABILITY_LABELS[capability]}</strong><small>{capability === 'chat' ? '文本、推理与工具调用' : '单次媒体 Provider 调用'}</small></span>
                      </label>
                    ))}
                  </div>
                </div>

                {enabledCapabilities.includes('chat') ? <div className="ai-capability-card">
                  <div className="ai-capability-card-head"><strong>对话</strong><span>HiMind 对话和工具分发使用</span></div>
                  <div className="ai-service-form-grid">
                    <label className="field-label ai-service-field"><span>对话默认模型</span><input value={draft.model} onChange={(event) => setDraft((current) => ({ ...current, model: event.target.value }))} placeholder="如 gpt-4.1 / deepseek-chat" /></label>
                    <label className="field-label ai-service-field"><span>可用模型</span><input value={draft.models} onChange={(event) => setDraft((current) => ({ ...current, models: event.target.value }))} placeholder="多个模型用逗号分隔" /></label>
                  </div>
                  <button className="btn ai-service-fetch-models" title={!draft.api_key.trim() && !editing ? '请输入 API Key 后再获取模型' : '获取服务提供的模型列表'} disabled={fetchingModels || !draft.base_url.trim() || (!draft.api_key.trim() && !editing)} onClick={() => void fetchModels()}>
                    {fetchingModels ? <BusyIndicator size={14} /> : <RefreshCw size={14} />}{fetchingModels ? '获取中' : '从服务获取模型'}
                  </button>
                </div> : null}

                {enabledCapabilities.includes('image.generate') ? <div className="ai-capability-card">
                  <div className="ai-capability-card-head"><strong>生图</strong><span>文本生成图片</span></div>
                  <div className="ai-service-form-grid">
                    <label className="field-label ai-service-field"><span>生图模型</span><input value={draft.image_model} onChange={(event) => setDraft((current) => ({ ...current, image_model: event.target.value }))} placeholder="如 gpt-image-1 / doubao-seedream-4-0" /></label>
                    <label className="field-label ai-service-field"><span>生图适配器</span><select value={draft.image_adapter || 'openai-images'} onChange={(event) => setDraft((current) => ({ ...current, image_adapter: event.target.value }))}><option value="openai-images">OpenAI Images</option><option value="ark-images">火山方舟</option></select></label>
                  </div>
                </div> : null}

                {enabledCapabilities.includes('image.edit') ? <div className="ai-capability-card">
                  <div className="ai-capability-card-head"><strong>改图</strong><span>参考图编辑 / 图生图</span></div>
                  <div className="ai-service-form-grid">
                    <label className="field-label ai-service-field"><span>改图模型</span><input value={draft.image_edit_model} onChange={(event) => setDraft((current) => ({ ...current, image_edit_model: event.target.value }))} placeholder="如 gpt-image-1 / doubao-seededit" /></label>
                    <label className="field-label ai-service-field"><span>改图适配器</span><select value={draft.image_edit_adapter || 'openai-images-edit'} onChange={(event) => setDraft((current) => ({ ...current, image_edit_adapter: event.target.value }))}><option value="openai-images-edit">OpenAI Images Edits</option><option value="ark-images">火山方舟</option></select></label>
                  </div>
                </div> : null}

                {enabledCapabilities.includes('video.generate') ? <div className="ai-capability-card">
                  <div className="ai-capability-card-head"><strong>视频</strong><span>文生视频 / 图生视频</span></div>
                  <div className="ai-service-form-grid">
                    <label className="field-label ai-service-field"><span>生视频模型</span><input value={draft.video_model} onChange={(event) => setDraft((current) => ({ ...current, video_model: event.target.value }))} placeholder="如 doubao-seedance-2-5-…" /></label>
                    <label className="field-label ai-service-field"><span>视频适配器</span><select value={draft.video_adapter || 'ark-video'} onChange={(event) => setDraft((current) => ({ ...current, video_adapter: event.target.value }))}><option value="ark-video">火山方舟 Seedance</option><option value="wan-video">阿里云百炼万相</option></select></label>
                  </div>
                  <small className="ai-service-hint">视频是异步任务，提交后通过媒体任务查询结果。</small>
                </div> : null}

                {enabledCapabilities.includes('audio.speech') ? <div className="ai-capability-card">
                  <div className="ai-capability-card-head"><strong>语音合成</strong><span>单次 TTS Provider 调用</span></div>
                  <div className="ai-service-form-grid">
                    <label className="field-label ai-service-field"><span>语音合成模型</span><input value={draft.speech_model} onChange={(event) => setDraft((current) => ({ ...current, speech_model: event.target.value }))} placeholder="如 tts-1 / s2.1-pro-free" /></label>
                    <label className="field-label ai-service-field"><span>语音合成适配器</span><select value={draft.speech_adapter || 'openai-speech'} onChange={(event) => setDraft((current) => ({ ...current, speech_adapter: event.target.value }))}><option value="openai-speech">OpenAI 兼容</option><option value="fish-audio-tts">Fish Audio</option></select></label>
                  </div>
                </div> : null}

                {enabledCapabilities.includes('audio.transcribe') ? <div className="ai-capability-card">
                  <div className="ai-capability-card-head"><strong>语音转写</strong><span>单次 ASR Provider 调用</span></div>
                  <div className="ai-service-form-grid">
                    <label className="field-label ai-service-field"><span>语音转写模型</span><input value={draft.transcribe_model} onChange={(event) => setDraft((current) => ({ ...current, transcribe_model: event.target.value }))} placeholder="如 whisper-1 / transcribe-1-pro" /></label>
                    <label className="field-label ai-service-field"><span>语音转写适配器</span><select value={draft.transcribe_adapter || 'openai-audio'} onChange={(event) => setDraft((current) => ({ ...current, transcribe_adapter: event.target.value }))}><option value="openai-audio">OpenAI 兼容</option><option value="fish-audio-asr">Fish Audio</option></select></label>
                  </div>
                </div> : null}
              </> : null}

              {formStep === 2 ? <>
                <div className="ai-service-form-group">
                  <div className="ai-service-group-label">高级</div>
                  <div className="ai-service-form-grid">
                    <label className="field-label ai-service-field"><span>服务 ID</span><input value={draft.id} disabled={editing} onChange={(event) => setDraft((current) => ({ ...current, id: event.target.value }))} placeholder={`自动生成：${suggestedServiceId(draft.display_name)}`} /></label>
                    <label className="field-label ai-service-field ai-service-field-wide"><span>接口类型</span><select value={draft.protocol} onChange={(event) => setDraft((current) => ({ ...current, protocol: event.target.value as AIServiceProtocol }))}>{protocolOptions.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}</select></label>
                  </div>
                  <small className="ai-service-hint">服务 ID 用于内部引用；新建时留空会自动生成。</small>
                </div>

                <div className="ai-service-form-group">
                  <div className="ai-service-group-label">用量与计费</div>
                  <div className="ai-service-form-grid">
                    <label className="field-label ai-service-field ai-service-field-wide"><span>余额/额度查询模板</span><select value={draft.balance_profile_id} onChange={(event) => setDraft((current) => ({ ...current, balance_profile_id: event.target.value }))}>
                      <option value="">不查询账户余额</option>
                      {balanceProfileOptions.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
                    </select></label>
                  </div>
                  {!draft.balance_profile_id && inferBalanceProfile(draft.base_url) ? <div className="ai-service-balance-suggestion">
                    <small className="ai-service-hint">按 HTTPS 主机建议：{balanceProfileOptions.find((option) => option.value === inferBalanceProfile(draft.base_url))?.label}。不会自动发起网络请求。</small>
                    <button type="button" className="btn" onClick={() => setDraft((current) => ({ ...current, balance_profile_id: inferBalanceProfile(current.base_url) ?? '' }))}>应用建议</button>
                  </div> : null}
                  {draft.balance_profile_id === 'new-api' ? <div className="ai-service-form-grid">
                    <label className="field-label ai-service-field"><span>New API Access Token（可留空使用 API Key）</span><input type="password" value={draft.balance_access_token} onChange={(event) => setDraft((current) => ({ ...current, balance_access_token: event.target.value }))} placeholder={editing ? '留空以保留当前 Token' : 'Access Token'} /></label>
                    <label className="field-label ai-service-field"><span>New API User ID</span><input value={draft.balance_user_id} onChange={(event) => setDraft((current) => ({ ...current, balance_user_id: event.target.value }))} placeholder="用户 ID" /></label>
                  </div> : null}
                  <small className="ai-service-hint">仅手动查询；请求留在服务同源。</small>
                </div>
              </> : null}

              <div className="modal-actions">
                <button className="btn" disabled={saving} onClick={formStep === 0 ? closeForm : goBack}>{formStep === 0 ? '取消' : <><ChevronLeft size={14} />上一步</>}</button>
                {formStep < 2 ? <button className="btn btn-primary" disabled={saving} onClick={goNext}>下一步<ChevronRight size={14} /></button> : <button className="btn btn-primary" disabled={saving} onClick={() => void saveService()}>{saving ? '保存中...' : editing ? '保存修改' : '保存服务'}</button>}
              </div>
            </div>
          </section>
        </div>
      ) : null}
    </div>
  );
}
function SourceUsageChips({ clients }: { clients: AIProviderImportStatus[] }) {
  if (!clients.length) return null;
  const names = clients.map((client) => clientLabel(client.target));
  const shown = names.slice(0, 4);
  return (
    <span className="ai-service-imported-clients" title={`已分发：${names.join('、')}`}>
      {shown.map((name) => <span className="ai-service-client-chip" key={name}>{name}</span>)}
      {names.length > shown.length ? <span className="ai-service-client-chip more">+{names.length - shown.length}</span> : null}
    </span>
  );
}

function formatBalanceAmount(value: number, currency: string) {
  const amount = Number.isFinite(value) ? value.toFixed(2) : '—';
  if (currency === 'CNY') return `¥${amount}`;
  if (currency === 'USD') return `$${amount}`;
  return `${amount} ${currency}`;
}

function formatBalanceObservedAt(timestamp: string) {
  const seconds = Number(timestamp);
  const date = Number.isFinite(seconds) && seconds > 0
    ? new Date(seconds * 1000)
    : new Date(timestamp);
  return Number.isNaN(date.getTime()) ? '时间未知' : date.toLocaleString();
}

function balanceComponentLabel(key: string) {
  const labels: Record<string, string> = {
    granted_balance: '赠送余额',
    topped_up_balance: '充值余额',
    cash_balance: '现金余额',
    voucher_balance: '代金券余额',
  };
  return labels[key] ?? key;
}
function CustomSourceRow({ service, active, capabilities, routeBadges, health, connectionCheck, clientStatuses, balanceSnapshot, balanceLoading, balanceBusy, balanceError, onCheckConnection, onRefreshBalance, onEdit, onRemove }: {
  service: CustomAIService;
  active: boolean;
  capabilities: AIServiceCapability[];
  routeBadges: string[];
  health: { text: string; kind: 'success' | 'warn' | 'neutral' };
  connectionCheck?: ConnectionCheck;
  clientStatuses: AIProviderImportStatus[];
  balanceSnapshot?: AIServiceBalanceSnapshot;
  balanceLoading: boolean;
  balanceBusy: boolean;
  balanceError: string;
  onCheckConnection: () => void;
  onRefreshBalance: () => void;
  onEdit: () => void;
  onRemove: () => void;
}) {
  const confirm = useConfirm();
  const canDelete = clientStatuses.length === 0;
  const hasChat = capabilities.includes('chat');
  const summary = `${service.base_url} · ${protocolLabels[service.protocol] ?? service.protocol} · ${hasChat ? `对话模型 ${service.model}` : '媒体服务'}`;
  const distribution = clientStatuses.length ? `已分发 ${clientStatuses.length} 个工具` : '未分发到工具';
  return (
    <article className={`ai-client-row ai-service-row${active ? ' active' : ''}`}>
      <div className="ai-client-icon target"><PlugZap size={18} /></div>
      <div className="ai-client-copy">
        <strong>{service.display_name}</strong>
        <span title={summary}>{summary}</span>
        <div className="ai-service-capability-chips" aria-label={`${service.display_name} 支持的能力`}>
          {capabilities.map(capability => <span key={capability}>{AI_SERVICE_CAPABILITY_LABELS[capability]}</span>)}
        </div>
        <div className="ai-service-state-chips">
          <span className={`ai-service-state ${health.kind}`}>{health.text}</span>
          {routeBadges.map(label => <span key={label} className="ai-service-state route">{label}</span>)}
          <span className="ai-service-state neutral">{distribution}</span>
        </div>
        {connectionCheck ? <small className={`ai-service-connection-check ${connectionCheck.state}`} aria-live="polite">{connectionCheck.message}</small> : null}
        {!service.balance_profile_id && service.suggested_balance_profile_id ? <small className="ai-service-balance-suggestion-label">检测建议：{balanceProfileOptions.find((option) => option.value === service.suggested_balance_profile_id)?.label ?? '手动配置'}</small> : null}
        {service.balance_profile_id ? (
          <div className="ai-service-balance" aria-live="polite">
            <div className="ai-service-balance-copy">
              {balanceSnapshot?.kind === 'quota' ? <>
                <small className="ai-service-balance-label">{balanceSnapshot.plan_name ?? '套餐配额'}</small>
                {balanceSnapshot.quota_windows.map((window) => <span key={window.name} title={window.resets_at ? `重置时间 ${formatBalanceObservedAt(window.resets_at)}` : undefined}>
                  {window.name} 剩余 {window.remaining_percent.toFixed(0)}%
                </span>)}
              </> : balanceSnapshot ? balanceSnapshot.balances.map((balance, index) => (
                <span key={`${balance.currency}-${index}`} title={Object.entries(balance.components ?? {}).map(([key, value]) => `${balanceComponentLabel(key)} ${formatBalanceAmount(value, balance.currency)}`).join(' · ')}>
                  {balanceSnapshot.kind === 'usage' ? <><small className="ai-service-balance-label">{balanceSnapshot.plan_name ? `${balanceSnapshot.plan_name} · ` : ''}剩余</small> </> : <small className="ai-service-balance-label">余额 </small>}{balanceSnapshot.is_available === false ? '账户不可用 · ' : ''}{formatBalanceAmount(balance.available, balance.currency)}
                </span>
              )) : <><small className="ai-service-balance-label">余额/额度</small><span>{balanceLoading ? '正在查询…' : '尚未查询'}</span></>}
              {balanceSnapshot?.observed_at ? <small>更新于 {formatBalanceObservedAt(balanceSnapshot.observed_at)}</small> : null}
              {balanceError ? <small className="ai-service-balance-error" role="status">{balanceError}</small> : null}
            </div>
            <button type="button" className="btn ai-service-balance-refresh" onClick={onRefreshBalance} disabled={balanceBusy} aria-label={`${service.display_name} 查询余额`}>
              {balanceLoading ? <BusyIndicator size={12} /> : <RefreshCw size={12} />}{balanceSnapshot ? '刷新' : '查询'}
            </button>
          </div>
        ) : null}
        <SourceUsageChips clients={clientStatuses} />
      </div>
      <div className="ai-client-registration-actions">
        {canDelete ? null : <span className="ai-service-delete-note">有工具在用</span>}
        <button type="button" className="btn ai-service-tool-btn" disabled={connectionCheck?.state === 'busy' || !hasChat} title={hasChat ? '检查对话模型列表接口' : '媒体服务需通过能力路由实际调用验证'} onClick={onCheckConnection}>
          {connectionCheck?.state === 'busy' ? <BusyIndicator size={12} /> : <RefreshCw size={12} />}检查
        </button>
        <button type="button" className="btn ai-service-tool-btn" onClick={onEdit}><Pencil size={14} />编辑</button>
        <button type="button" className="btn ai-service-tool-btn" disabled={!canDelete} title={canDelete ? undefined : '有 AI 工具正在使用该服务，请先取消分发'} onClick={() => { void confirm({ title: `删除 AI 服务「${service.display_name}」？`, description: '删除后需要重新填写连接信息和密钥才能再使用。', confirmText: '删除' }).then((accepted) => { if (accepted) onRemove(); }); }}><Trash2 size={14} />删除</button>
      </div>
    </article>
  );
}
function ManagedSourceRow({ managed, active, clientStatuses, onOpenAccount, onRefresh }: {
  managed: ManagedAIServiceSummary;
  active: boolean;
  clientStatuses: AIProviderImportStatus[];
  onOpenAccount: () => void;
  onRefresh: () => void;
}) {
  if (managed.available) {
    const models = managed.models?.length ? `${managed.models.length} 个模型` : '未返回模型列表';
    const boundClients = clientStatuses.filter((client) => client.state === 'imported' && client.service === 'managed');
    return (
      <article className={`ai-client-row ai-service-row${active ? ' active' : ''}`}>
        <div className="ai-client-icon target"><ShieldCheck size={18} /></div>
        <div className="ai-client-copy">
          <strong>工作台模型服务</strong>
          <span>{managed.model} · {models} · {managed.base_url}</span>
          <SourceUsageChips clients={boundClients} />
        </div>
        <Pill kind={boundClients.length ? 'success' : 'neutral'}>{boundClients.length ? `已分发 ${boundClients.length} 个` : '未分发'}</Pill>
        <div className="ai-client-registration-actions">
          <span className="ai-target-managed"><ShieldCheck size={13} /> 工作台</span>
        </div>
      </article>
    );
  }
  const reason = managed.reason ?? 'unknown';
  const reasonText: Record<string, string> = {
    not_authorized: '工作台服务未连接',
    user_mismatch: '工作台账号不一致',
    independent: '未连接工作台服务',
    no_credential: '工作台服务未配置',
    not_ready: '工作台服务暂不可用',
    network_error: '工作台连接失败',
    dashboard_error: '工作台服务读取失败',
    parse_error: '工作台服务读取失败',
  };
  return (
    <section className="ai-managed-strip">
      <div className="ai-managed-icon muted">{reason === 'not_authorized' ? <CircleDashed size={18} /> : <CircleX size={18} />}</div>
      <div className="ai-managed-copy">
        <strong>{reasonText[reason] ?? '工作台服务不可用'}</strong>
        <span>{reason === 'not_authorized' || reason === 'user_mismatch' ? '连接账号后即可使用' : reason === 'network_error' || reason === 'dashboard_error' || reason === 'parse_error' ? '请稍后重试' : '请在工作台完成配置'}</span>
      </div>
      <div className="ai-managed-actions">
        {reason === 'not_authorized' || reason === 'user_mismatch' ? <button className="btn" onClick={onOpenAccount}><ExternalLink size={13} />连接账号</button> : null}
        {reason === 'no_credential' || reason === 'not_ready' ? <button className="btn" onClick={onOpenAccount}><ExternalLink size={13} />配置服务</button> : null}
        {reason === 'network_error' || reason === 'dashboard_error' || reason === 'parse_error' ? <button className="btn btn-icon" title="重新读取工作台服务" aria-label="重新读取工作台服务" onClick={onRefresh}><RefreshCw size={14} /></button> : null}
      </div>
    </section>
  );
}

function formatAIServiceError(error: unknown, fallback: string) {
  const detail = typeof error === 'string'
    ? error
    : error instanceof Error
      ? error.message
      : error && typeof error === 'object'
        ? (() => {
          const value = error as Record<string, unknown>;
          return typeof value.message === 'string' ? value.message : typeof value.error === 'string' ? value.error : '';
        })()
        : '';
  const normalized = detail.replace(/[\r\n]+/g, ' ').replace(/\s+/g, ' ').trim();
  if (!normalized) return fallback;
  return normalized.length > 240 ? `${normalized.slice(0, 237)}...` : normalized;
}
