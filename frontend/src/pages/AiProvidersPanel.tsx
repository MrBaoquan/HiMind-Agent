import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import './ServiceUi.css';
import { Check, ChevronLeft, ChevronRight, CircleDashed, CircleX, Copy, ExternalLink, Info, Pencil, PlugZap, RefreshCw, Search, ShieldCheck, Trash2, X } from 'lucide-react';
import { Pill } from '../components/Common';
import { BusyIndicator } from '../components/BusyIndicator';
import { ModalSurface } from '../components/ModalSurface';
import { useConfirm } from '../components/ConfirmDialog';
import { fallbackAiServicePresets, type AiServicePreset } from './aiServicePresets';
import { inferBalanceProfile } from './balanceProfileMatching';
import { clientLabel } from '../utils/clientLabels';
import { agentApi } from '../services/agentApi';
import { AiModelCatalogEditor } from './AiModelCatalogEditor';
import { emptyModelSetup, modelsFromPreset, modelsFromService, modelSetupError, modelConfiguration, mergeDiscoveredModels, configuredOperations } from './aiServiceOnboarding';
import type { AIProviderImportStatus, AIServiceBalanceSnapshot, AIServiceListResult, AIServiceProtocol, AIServiceTemplateListResult, CustomAIService, ManagedAIServiceSummary, MediaProviderCatalogResult } from '../services/agentApi';
import {
  AI_SERVICE_CAPABILITIES,
  AI_SERVICE_CAPABILITY_LABELS,
  MEDIA_CAPABILITIES,
  capabilityModel,
  primaryRouteServiceId,
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

/** 协议族：两种 OpenAI 兼容线格式共用同一 `/v1`，同一族只能有一条端点。 */
function protocolFamily(protocol: AIServiceProtocol): 'openai' | 'anthropic' {
  return protocol === 'anthropic' ? 'anthropic' : 'openai';
}

/** 第二端点里与主端点同族的选项没有意义（同一 `/v1` 只会被去重丢掉）。 */
function secondaryProtocolOptions(primary: AIServiceProtocol): Array<{ value: AIServiceProtocol; label: string }> {
  return protocolOptions.filter(option => protocolFamily(option.value) !== protocolFamily(primary));
}

/** 火山方舟 Agent Plan 只有 Messages 推理入口，没有 `/models` 发现接口。 */
function isVolcengineAgentPlan(baseUrl: string): boolean {
  try {
    const parsed = new URL(baseUrl.trim());
    const host = parsed.hostname.toLowerCase();
    const path = parsed.pathname.replace(/\/+$/, '').replace(/\/v1$/, '');
    return host.endsWith('.volces.com') && path === '/api/plan';
  } catch {
    return false;
  }
}

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
    /** 全部接入端点；省略时后端按 `protocol` + `base_url` 合成单端点。 */
    endpoints?: Array<{ protocol: AIServiceProtocol; base_url: string }>;
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
    /** 视觉理解专用模型；旧配置留空时跟随对话模型，新配置受 enabled_capabilities 限制。 */
    vision_model: string;
    /** 媒体能力的额外可选模型：能力 → 模型列表（不含默认模型）。 */
    media_models: Record<string, string[]>;
    enabled_capabilities: AIServiceCapability[];
    balance_profile_id: string;
    balance_access_token: string;
    balance_user_id: string;
    api_key: string;
    /** 新增时目标 ID 已存在：只有显式确认后才允许覆盖。 */
    allow_overwrite?: boolean;
  }) => Promise<void>;
  onRemoveAIService: (id: string) => Promise<void>;
  onCloneAIService: (id: string) => Promise<CustomAIService>;
  onOpenAccount: () => void;
  onOpenRoutes?: () => void;
  onFetchModels: (input: { base_url: string; api_key: string; protocol: AIServiceProtocol }) => Promise<string[]>;
  onFetchSavedModels: (id: string, base_url: string) => Promise<string[]>;
  onFetchBalance: (id: string) => Promise<AIServiceBalanceSnapshot>;
};

type FormStep = 0 | 1;
type ConnectionCheck = { state: 'busy' | 'ok' | 'error'; message: string };

const emptyDraft = {
  id: '',
  display_name: '',
  base_url: '',
  protocol: 'openai-responses' as AIServiceProtocol,
  // 第二条接入端点（可选）：同一账号在另一套线协议上的地址。
  // 主 Base URL + 接口类型是第一条端点；这里只补「另一个协议族」那一条。
  secondary_protocol: '' as AIServiceProtocol | '',
  secondary_base_url: '',
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
  onCloneAIService,
  onOpenAccount,
  onOpenRoutes,
  onFetchModels,
  onFetchSavedModels,
  onFetchBalance,
}: AiProvidersPanelProps) {
  const [formOpen, setFormOpen] = useState(false);
  const [formStep, setFormStep] = useState<FormStep>(0);
  const [editingServiceId, setEditingServiceId] = useState<string | null>(null);
  const [draft, setDraft] = useState(emptyDraft);
  const [modelSetup, setModelSetup] = useState(emptyModelSetup);
  const discoveryRevision = useRef(0);
  const [savedNotice, setSavedNotice] = useState('');
  const [saving, setSaving] = useState(false);
  const [fetchingModels, setFetchingModels] = useState(false);
  const [formError, setFormError] = useState('');
  const [formNotice, setFormNotice] = useState('');
  const [balanceSnapshots, setBalanceSnapshots] = useState<Record<string, AIServiceBalanceSnapshot>>(aiServices?.custom?.balances ?? {});
  const [providerCatalog, setProviderCatalog] = useState<MediaProviderCatalogResult | null>(null);
  const [balanceRefreshingId, setBalanceRefreshingId] = useState<string | null>(null);
  const [balanceErrors, setBalanceErrors] = useState<Record<string, string>>({});
  const [connectionChecks, setConnectionChecks] = useState<Record<string, ConnectionCheck>>({});
  const [cloningServiceId, setCloningServiceId] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState('');
  const [capabilityFilter, setCapabilityFilter] = useState<'all' | AIServiceCapability>('all');
  const [selectedPreset, setSelectedPreset] = useState<string>('');
  const stepTitleRef = useRef<HTMLHeadingElement>(null);
  const pendingFieldFocus = useRef<string | null>(null);
  const saveLock = useRef(false);
  const [fieldError, setFieldError] = useState<{ field: string; step: FormStep; message: string } | null>(null);
  const [catalogState, setCatalogState] = useState<'loading' | 'ready' | 'error' | 'denied'>('loading');
  const [catalogTime, setCatalogTime] = useState<string | null>(null);
  const [catalogRevision, setCatalogRevision] = useState(0);
  const confirm = useConfirm();

  const customServices = aiServices?.custom?.services ?? [];
  // 每个能力的默认服务即该能力路由表的第一项（ADR 0023）。
  const routeTable = aiServices?.custom?.capability_routes ?? {};
  const activeServiceId = primaryRouteServiceId(routeTable, 'chat');
  const managed = aiServices?.managed ?? { available: false };
  const clientStatuses = [...new Map((aiServices?.clients?.targets ?? []).map(client => [client.target, client])).values()];
  const serviceCount = customServices.length;
  const editing = editingServiceId !== null;

  useEffect(() => {
    setBalanceSnapshots(aiServices?.custom?.balances ?? {});
  }, [aiServices?.custom?.balances]);

  useEffect(() => {
    let canceled = false;
    setCatalogState('loading');
    void agentApi.getMediaProviderCatalog()
      .then((catalog) => { if (!canceled) { setProviderCatalog(catalog); setCatalogTime(new Date().toISOString()); setCatalogState('ready'); } })
      .catch((error) => { if (!canceled) setCatalogState(/403|forbidden|permission|无权限/i.test(String(error)) ? 'denied' : 'error'); });
    return () => { canceled = true; };
  }, [aiServices, catalogRevision]);

  useLayoutEffect(() => {
    if (formOpen && pendingFieldFocus.current) {
      focusField(pendingFieldFocus.current);
      pendingFieldFocus.current = null;
    } else if (formOpen) stepTitleRef.current?.focus({ preventScroll: true });
    else setFieldError(null);
  }, [formOpen, formStep]);

  useLayoutEffect(() => {
    if (formError && fieldError) document.getElementById('ai-service-error-link')?.focus();
  }, [formError, fieldError]);

  function serviceHealth(service: CustomAIService): { text: string; kind: 'neutral' | 'success' | 'warn' } {
    if (catalogState !== 'ready') return { text: '健康未知', kind: 'neutral' };
    const provider = providerCatalog?.providers.find(item => item.id === service.id);
    if (!provider) return { text: '健康未知', kind: 'neutral' };
    return serviceHealthLabel(provider, Boolean(provider.has_api_key));
  }

  function routeBadges(service: CustomAIService) {
    const labels: string[] = [];
    if (activeServiceId === service.id) labels.push('对话默认');
    for (const capability of MEDIA_CAPABILITIES) {
      if (primaryRouteServiceId(routeTable, capability) === service.id) {
        labels.push(`${AI_SERVICE_CAPABILITY_LABELS[capability]}默认`);
      }
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
    discoveryRevision.current++;
    setFetchingModels(false);
    setModelSetup(modelsFromPreset(preset));
    setSelectedPreset(presetId);
    setFormStep(0);
    setFormError('');
    setFormNotice('');
    setDraft({
      // 预设只是模板：不能把模板 ID 当成服务 ID，否则新增会覆盖同 ID 服务，
      // 编辑态还会把保存目标切到模板对应的服务上。
      ...emptyDraft,
      id: editingServiceId ?? draft.id,
      display_name: draft.display_name,
      base_url: preset.base_url,
      protocol: preset.protocol,
      secondary_protocol: '',
      secondary_base_url: '',
      balance_profile_id: balanceProfileForPreset(preset.base_url),
      balance_access_token: '',
      balance_user_id: '',
      // 切换供应商时清空当前输入的密钥；编辑态留空仍表示保留已保存 Key。
      api_key: selectedPreset === presetId ? draft.api_key : '',
    });
  }

  function updateConnection(field: 'base_url' | 'protocol' | 'api_key', value: string) {
    discoveryRevision.current++;
    setFetchingModels(false);
    setFormError('');
    setDraft(current => ({ ...current, [field]: value }));
    if (field === 'api_key') {
      setModelSetup(current => ({ ...current, entries: current.entries.filter(entry => entry.source !== 'discovered' || entry.selected)
        .map(entry => entry.source === 'discovered' ? { ...entry, source: 'retained' as const } : entry) }));
      setFormNotice('凭据已修改；请重新获取目录。已选模型保留，仍需实际调用验证。');
    } else {
      setSelectedPreset('');
      setModelSetup(emptyModelSetup());
      setFormNotice('连接已修改，请重新获取目录或添加模型。');
    }
  }

  function serviceName() {
    if (draft.display_name.trim()) return draft.display_name.trim();
    const presetName = presetEntries.find(item => item.id === selectedPreset)?.name;
    if (presetName) return presetName;
    try { return new URL(draft.base_url).hostname; } catch { return ''; }
  }

  function connectionStepError() {
    if (!editing && !draft.api_key.trim()) return '请先填写「API Key」。';
    if (!draft.base_url.trim()) return '请先填写「Base URL」。';
    try {
      const url = new URL(draft.base_url.trim());
      if (!['http:', 'https:'].includes(url.protocol) || url.search || url.hash || url.username || url.password) return 'Base URL 需为不含查询参数或账号的 HTTP/HTTPS API 地址。';
    } catch { return 'Base URL 不是有效的 API 地址。'; }
    if (draft.secondary_base_url.trim()) {
      try {
        const url = new URL(draft.secondary_base_url.trim());
        if (!['http:', 'https:'].includes(url.protocol) || url.search || url.hash || url.username || url.password) return '第二接入端点需为不含查询参数或账号的 HTTP/HTTPS API 地址。';
      } catch { return '第二接入端点不是有效的 API 地址。'; }
    }
    if (Boolean(draft.secondary_protocol) !== Boolean(draft.secondary_base_url.trim())) return '第二接入端点需要同时填写地址和接口类型。';
    if (draft.secondary_protocol && protocolFamily(draft.secondary_protocol) === protocolFamily(draft.protocol)) return '第二接入端点需使用不同的接口协议族。';
    return '';
  }

  function capabilityStepError() { return modelSetupError(modelSetup); }

  function goNext() {
    const error = formStep === 0 ? connectionStepError() : capabilityStepError();
    if (error) {
      showValidation(error);
      return;
    }
    setFormError('');
    setFieldError(null);
    setFormStep(1);
    void fetchModels();
  }

  function goBack() {
    setFormError('');
    setFieldError(null);
    setFormStep(current => Math.max(0, current - 1) as FormStep);
  }

  function showValidation(message: string, sourceField?: string) {
    const requestedField = sourceField ?? (message.includes('API Key') ? 'api_key'
      : message.includes('第二接入端点') ? 'secondary_base_url'
      : message.includes('Base URL') ? 'base_url'
      : message.includes('User ID') ? 'balance_user_id' : 'capabilities');
    const field = requestedField === 'model' || requestedField.endsWith('_model') ? 'capabilities' : requestedField;
    const step: FormStep = ['display_name', 'api_key', 'base_url', 'secondary_base_url'].includes(field) ? 0 : 1;
    setFieldError({ field, step, message });
    setFormError(message);
  }

  function fieldProps(field: string) {
    return { id: `ai-service-${field}`, 'aria-invalid': fieldError?.field === field || undefined,
      'aria-describedby': fieldError?.field === field ? 'ai-service-field-error' : undefined };
  }

  function focusField(field: string) {
    const element = document.getElementById(`ai-service-${field}`);
    for (let parent = element?.parentElement; parent; parent = parent.parentElement) {
      if (parent instanceof HTMLDetailsElement) parent.open = true;
    }
    element?.focus();
  }

  function locateError() {
    if (!fieldError) return;
    if (formStep === fieldError.step) focusField(fieldError.field);
    else { pendingFieldFocus.current = fieldError.field; setFormStep(fieldError.step); }
  }

  async function saveService() {
    if (saveLock.current) return;
    saveLock.current = true;
    try { await performSave(); } finally { saveLock.current = false; }
  }

  async function performSave() {
    const error = connectionStepError() || capabilityStepError();
    if (error) {
      showValidation(error);
      return;
    }
    if (draft.balance_profile_id === 'new-api' && !draft.balance_user_id.trim()) {
      showValidation('New API 余额查询需要填写 User ID。');
      return;
    }
    const configuration = modelConfiguration(modelSetup);
    // 编辑态永远保存回当前服务；新增态才按用户填写/自动生成的 ID 落库。
    let targetId = editingServiceId ?? (draft.id.trim() || suggestedServiceId(serviceName()));
    if (!editingServiceId && !draft.id.trim()) {
      const stem = targetId; let suffix = 2;
      while (customServices.some(service => service.id === targetId)) targetId = `${stem}-${suffix++}`;
    }
    const existingById = !editingServiceId && customServices.some(service => service.id === targetId);
    const normalizedName = serviceName().toLowerCase();
    const nameCollision = normalizedName.length > 0 && customServices.some(service =>
      service.id !== targetId && service.display_name.trim().toLowerCase() === normalizedName
    );
    let allowOverwrite = Boolean(editingServiceId);
    if (existingById || nameCollision) {
      const conflicts = [
        existingById ? `服务 ID「${targetId}」已存在` : '',
        nameCollision ? `服务名称「${draft.display_name.trim()}」已被其他服务使用` : '',
      ].filter(Boolean).join('；');
      const accepted = await confirm({
        title: '保存前确认冲突',
        description: existingById
          ? `${conflicts}。继续会用当前表单覆盖该服务的连接信息、模型和能力；已分发客户端的绑定不会自动迁移。`
          : `${conflicts}。继续会创建同名服务，列表和聚合入口里可能难以区分。`,
        confirmText: existingById ? '覆盖保存' : '仍然保存',
        danger: existingById,
      });
      if (!accepted) return;
      allowOverwrite = allowOverwrite || existingById;
    }
    setFormError('');
    setSaving(true);
    try {
      await onSaveAIService({
        id: targetId,
        display_name: serviceName(),
        base_url: draft.base_url.trim(),
        protocol: draft.protocol,
        // 只提交「另一个协议族」的端点；主端点由 protocol + base_url 表达，
        // 后端会把它补成 endpoints[0]。第二行留空就不传，旧行为不变。
        endpoints: draft.secondary_base_url.trim() && draft.secondary_protocol
          ? [
              { protocol: draft.protocol, base_url: draft.base_url.trim() },
              { protocol: draft.secondary_protocol, base_url: draft.secondary_base_url.trim() },
            ]
          : undefined,
        ...configuration,
        balance_profile_id: draft.balance_profile_id,
        balance_access_token: draft.balance_access_token.trim(),
        balance_user_id: draft.balance_user_id.trim(),
        api_key: draft.api_key.trim(),
        allow_overwrite: allowOverwrite,
      });
      setDraft(emptyDraft);
      setModelSetup(emptyModelSetup());
      setSavedNotice('服务配置已保存，尚未验证全部模型。全局默认路由保持原设置；未指定默认时仍按现有自动规则选择。');
      discoveryRevision.current++;
      setFormStep(0);
      setEditingServiceId(null);
      setSelectedPreset('');
      setFormOpen(false);
    } catch (error) {
      const message = formatAIServiceError(error, '保存 AI 服务失败，请检查连接信息后重试。');
      const sourceField = ['id', 'display_name', 'api_key', 'base_url', 'balance_user_id', 'image_edit_model', 'image_model', 'video_model', 'speech_model', 'transcribe_model', 'vision_model', 'model']
        .find(field => new RegExp(`\\b${field}\\b`).test(message));
      if (sourceField) showValidation(message, sourceField);
      else { setFieldError(null); setFormError(message); }
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
    const turn = ++discoveryRevision.current;
    setFormError(''); setFormNotice('');
    if (isVolcengineAgentPlan(draft.base_url) || selectedPreset === 'wan-video') {
      setFormNotice('此接入方式未提供通用模型目录，已显示预设建议；也可手动添加。模型尚未验证。');
      return;
    }
    const saved = customServices.find(service => service.id === editingServiceId);
    if (!draft.api_key.trim() && saved && (saved.base_url !== draft.base_url.trim() || saved.protocol !== draft.protocol)) {
      setFormNotice('连接地址或接口已修改；请填写 API Key 后获取新目录，避免使用旧连接的模型列表。');
      return;
    }
    setFetchingModels(true);
    try {
      const models = draft.api_key.trim()
        ? await onFetchModels({ base_url: draft.base_url.trim(), api_key: draft.api_key.trim(), protocol: draft.protocol })
        : await onFetchSavedModels(editingServiceId!, draft.base_url.trim());
      if (turn !== discoveryRevision.current) return;
      setModelSetup(current => mergeDiscoveredModels(current, models));
      setFormNotice(`已获取 ${models.length} 个目录模型。`);
    } catch (error) {
      if (turn !== discoveryRevision.current) return;
      setFormNotice(formatAIServiceError(error, '无法获取模型目录。可重试，或按供应商说明手动添加模型。') + ' 可重试或手动添加。');
    } finally {
      if (turn === discoveryRevision.current) setFetchingModels(false);
    }
  }

  async function checkServiceConnection(service: CustomAIService) {
    setConnectionChecks(current => ({ ...current, [service.id]: { state: 'busy', message: '检查中…' } }));
    try {
      const models = await onFetchSavedModels(service.id, service.base_url);
      setConnectionChecks(current => ({
        ...current,
        [service.id]: {
          state: 'ok',
          message: isVolcengineAgentPlan(service.base_url)
            ? `推理接口可用 · 已验证 ${capabilityModel(service, 'chat') || '默认模型'} · ${models.length} 个目录模型`
            : `模型接口可用 · ${models.length} 个模型`,
        },
      }));
    } catch (error) {
      setConnectionChecks(current => ({
        ...current,
        [service.id]: {
          state: 'error',
          message: formatAIServiceError(
            error,
            isVolcengineAgentPlan(service.base_url) ? '推理接口不可用' : '模型接口不可用',
          ),
        },
      }));
    }
  }

  async function cloneService(service: CustomAIService) {
    if (cloningServiceId) return;
    setCloningServiceId(service.id);
    setConnectionChecks(current => ({
      ...current,
        [service.id]: { state: 'busy', message: '正在复制 AI 服务和加密凭据…' },
    }));
    try {
      const cloned = await onCloneAIService(service.id);
      setConnectionChecks(current => ({
        ...current,
        [service.id]: { state: 'ok', message: `AI 服务已复制为 ${cloned.display_name}` },
      }));
      openEditService(cloned);
      setFormNotice('已复制连接信息、模型、能力与加密凭据。可修改名称或参数后保存为新服务。');
    } catch (error) {
      setConnectionChecks(current => ({
        ...current,
        [service.id]: {
          state: 'error',
          message: formatAIServiceError(error, '复制 AI 服务失败'),
        },
      }));
    } finally {
      setCloningServiceId(null);
    }
  }

  function openNewService() {
    setSelectedPreset('');
    setEditingServiceId(null);
    setDraft(emptyDraft);
    setModelSetup(emptyModelSetup());
    setSavedNotice('');
    discoveryRevision.current++;
    setFetchingModels(false);
    setFormStep(0);
    setFormError('');
    setFormNotice('');
    setFormOpen(true);
  }

  function openEditService(service: CustomAIService) {
    setSelectedPreset('');
    setEditingServiceId(service.id);
    setDraft({
      id: service.id,
      display_name: service.display_name,
      base_url: service.base_url,
      protocol: service.protocol,
      // 「主端点」与「第二端点」按协议族分工：第一条是 base_url/protocol，
      // 另一族的端点回填到第二行。旧服务没有 endpoints 时第二行为空。
      ...(() => {
        const secondary = (service.endpoints ?? []).slice(1).find(
          endpoint => protocolFamily(endpoint.protocol) !== protocolFamily(service.protocol),
        );
        return {
          secondary_protocol: secondary?.protocol ?? ('' as AIServiceProtocol | ''),
          secondary_base_url: secondary?.base_url ?? '',
        };
      })(),
      balance_profile_id: service.balance_profile_id ?? '',
      balance_access_token: '',
      balance_user_id: service.balance_user_id ?? '',
      api_key: '',
    });
    setModelSetup(modelsFromService(service));
    discoveryRevision.current++;
    setFetchingModels(false);
    setFormStep(0);
    setFormError('');
    setFormNotice('');
    setFormOpen(true);
  }

  function closeForm() {
    if (saving) return;
    discoveryRevision.current++;
    setFetchingModels(false);
    setFormOpen(false);
    setEditingServiceId(null);
    setFormStep(0);
    setFormNotice('');
  }
  return (
    <div className="ai-providers-view">
      {savedNotice ? <div className="ai-save-result" role="status"><span>{savedNotice}</span>{onOpenRoutes ? <button type="button" className="btn" onClick={onOpenRoutes}>设置默认与备用</button> : null}<button type="button" className="btn" onClick={() => setSavedNotice('')}>知道了</button></div> : null}
      <div className="ai-catalog-state" role={catalogState === 'error' || catalogState === 'denied' ? 'alert' : 'status'} aria-busy={catalogState === 'loading'}>
        {catalogState === 'loading' ? '正在读取健康目录，AI 服务配置仍可操作。' : catalogState === 'denied' ? '无权限读取健康目录；请联系管理员授权，配置列表不受影响。' : catalogState === 'error' ? '健康目录读取失败；健康状态未知，配置列表不受影响。' : providerCatalog?.providers.some(provider => provider.health.state === 'degraded' || provider.health.state === 'unavailable') ? '健康目录已更新，部分 AI 服务不可用；请检查对应连接。' : '健康目录已更新；可逐项检查 AI 服务连接。'}
        <span>{catalogTime ? `最近成功快照：${catalogTime}` : '尚无成功快照'}</span>
        {catalogState === 'error' || catalogState === 'denied' ? <button className="btn" onClick={() => setCatalogRevision(value => value + 1)}>重试健康目录</button> : null}
      </div>
      <section className={`ai-overview ${serviceCount ? 'ready' : 'attention'}`}>
        <div className="ai-overview-main">
          <div className="ai-overview-icon"><PlugZap size={20} /></div>
          <div className="ai-overview-copy">
            <span className="ai-overview-eyebrow">AI 服务</span>
            <strong>{serviceCount ? `${serviceCount} 个本机 AI 服务` : '还没有可用的 AI 服务'}</strong>
            <span>一个服务可同时提供对话、图像、视频和音频模型；默认路由在「能力路由」查看。</span>
          </div>
        </div>
        <div className="ai-overview-actions">
          <button className="btn btn-primary" onClick={openNewService}>新增 AI 服务</button>
        </div>
      </section>

      <section className="ai-managed-section" aria-labelledby="ai-managed-title">
        <div className="ai-section-heading">
          <div><h3 id="ai-managed-title">工作台 AI 服务</h3><span>由 AI 工作台管理，不计入本机 AI 服务数量。</span></div>
        </div>
        <ManagedSourceRow managed={managed} active={!activeServiceId} clientStatuses={clientStatuses} onOpenAccount={onOpenAccount} onRefresh={onRefresh} />
      </section>

      <section className="ai-client-section" aria-labelledby="ai-services-title">
        <div className="ai-section-heading">
          <div><h3 id="ai-services-title">本机 AI 服务</h3><span>连接、凭据和能力在这里管理；路由与工具分发在各自页面完成。</span></div>
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
              cloneBusy={cloningServiceId === service.id}
              onCheckConnection={() => void checkServiceConnection(service)}
              onRefreshBalance={() => void refreshBalance(service.id)}
              onClone={() => void cloneService(service)}
              onEdit={() => openEditService(service)}
              onRemove={() => void onRemoveAIService(service.id)}
            />
          ))}
          {!serviceCount ? <div className="ai-empty-row" role="status"><span className="ai-empty-icon"><CircleDashed size={14} /></span><span>{aiServices ? '暂无本机 AI 服务，点「新增 AI 服务」连接一个模型来源。' : '正在读取 AI 服务配置，请稍候；若持续无响应请刷新。'}</span></div> : null}
          {serviceCount && !filteredServices.length ? <div className="ai-empty-row"><span className="ai-empty-icon"><Search size={14} /></span><span>没有匹配的服务，换一个关键词或能力筛选。</span></div> : null}
        </div>
      </section>

      {formOpen ? (
        <ModalSurface className="modal ai-service-modal" titleId="ai-service-modal-title" onClose={closeForm}>
          <div aria-busy={saving}>
            <div className="modal-header">
              <div>
                <h3 id="ai-service-modal-title" ref={stepTitleRef} tabIndex={-1} data-initial-focus>{editing ? '编辑 AI 服务' : '新增 AI 服务'} · {['连接服务', '选择模型'][formStep]}</h3>
                <p>连接信息和密钥只保存在本机。</p>
              </div>
              <button className="btn btn-icon" title="关闭" aria-label="关闭" disabled={saving} onClick={closeForm}><X size={16} /></button>
            </div>
            <div className="modal-body ai-service-modal-body">
              {formError ? <div className="ai-service-form-error" role="alert"><CircleX size={15} /><span id="ai-service-field-error">{formError}{fieldError ? <a id="ai-service-error-link" href={`#ai-service-${fieldError.field}`} onClick={event => { event.preventDefault(); locateError(); }}>定位并修正字段</a> : null}</span></div> : null}
              {formNotice ? <div className="ai-service-form-notice" role="status"><Info size={15} /><span>{formNotice}</span></div> : null}
              <div className="ai-service-steps" aria-label="配置步骤">
                {['连接服务', '选择模型'].map((label, index) => (
                  <span key={label} aria-current={formStep === index ? 'step' : undefined} className={`ai-service-step${formStep === index ? ' active' : ''}${formStep > index ? ' complete' : ''}`}>
                    <b>{formStep > index ? <Check size={12} /> : index + 1}</b>{label}
                  </span>
                ))}
              </div>

              {formStep === 0 ? <>
                <div className="ai-service-form-group">
                  <label className="field-label ai-service-field"><span>服务供应商</span><select value={selectedPreset} onChange={event => {
                    if (event.target.value) applyPreset(event.target.value);
                    else { discoveryRevision.current++; setFetchingModels(false); setSelectedPreset(''); setDraft({ ...emptyDraft, id: editingServiceId ?? '', display_name: draft.display_name }); setModelSetup(emptyModelSetup()); setFormNotice(''); }
                  }}><option value="">自定义 / OpenAI 兼容服务</option>{[...new Set(presetEntries.map(item => item.category))].map(category => <optgroup key={category} label={category}>{presetEntries.filter(item => item.category === category).map(preset => <option key={preset.id} value={preset.id}>{preset.name}</option>)}</optgroup>)}</select></label>
                  <small className="ai-service-hint">同一个服务可提供对话、图像、视频和音频模型。</small>
                </div>
                <div className="ai-service-form-group">
                  <div className="ai-service-group-label">连接</div>
                  <div className="ai-service-form-grid">
                    <label className="field-label ai-service-field ai-service-field-wide"><span>服务名称（可选）</span><input {...fieldProps('display_name')} value={draft.display_name} onChange={(event) => setDraft((current) => ({ ...current, display_name: event.target.value }))} placeholder={presetEntries.find(item => item.id === selectedPreset)?.name || '默认使用连接地址的主机名'} /></label>
                    <label className="field-label ai-service-field ai-service-field-wide"><span>API Key</span><input {...fieldProps('api_key')} type="password" value={draft.api_key} onChange={event => updateConnection('api_key', event.target.value)} placeholder={editing ? '留空以保留当前 Key' : 'sk-...'} /></label>
                    {selectedPreset ? <div className="ai-connection-summary">{draft.base_url}<br />{protocolLabels[draft.protocol]}</div> : null}
                    <details className="ai-onboarding-details ai-service-field-wide" open={!selectedPreset}><summary>连接地址与接口</summary>
                    <label className="field-label ai-service-field"><span>Base URL</span><input {...fieldProps('base_url')} value={draft.base_url} onChange={event => updateConnection('base_url', event.target.value)} placeholder="https://api.example.com/v1" /></label>
                    <label className="field-label ai-service-field"><span>接口类型</span><select aria-label="接口类型" value={draft.protocol} onChange={event => updateConnection('protocol', event.target.value)}>{protocolOptions.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}</select></label>
                    </details>
                  </div>
                  <small className="ai-service-hint">接口按供应商说明填写；获取目录不会发起模型推理。高级媒体调用方式可在下一步调整。</small>
                </div>

                <details className="ai-onboarding-details">
                  <summary>高级连接：第二接入端点</summary>
                  <div className="ai-service-form-grid">
                    <label className="field-label ai-service-field ai-service-field-wide"><span>第二 Base URL</span><input {...fieldProps('secondary_base_url')} value={draft.secondary_base_url} onChange={(event) => setDraft((current) => ({ ...current, secondary_base_url: event.target.value }))} placeholder="https://api.example.com/anthropic" /></label>
                    <label className="field-label ai-service-field"><span>第二接口类型</span><select value={draft.secondary_protocol} onChange={(event) => setDraft((current) => ({ ...current, secondary_protocol: event.target.value as AIServiceProtocol | '' }))}><option value="">不配置</option>{secondaryProtocolOptions(draft.protocol).map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}</select></label>
                  </div>
                  <small className="ai-service-hint">同一账号和凭据的另一种协议地址；留空时只使用主端点。</small>
                </details>
              </> : null}

              {formStep === 1 ? <>
                <div className="ai-model-catalog-heading"><div><strong>选择此服务的模型</strong><p className="ai-service-hint">目录只表示模型已列出；用途是配置选择，实际可用性需调用验证。</p></div>
                  <button type="button" className="btn" disabled={fetchingModels || saving} onClick={() => void fetchModels()}>{fetchingModels ? <><BusyIndicator size={14} />正在获取目录…</> : '从服务获取模型'}</button>
                </div>
                <AiModelCatalogEditor value={modelSetup} onChange={setModelSetup} disabled={saving} />
                <details className="ai-onboarding-details"><summary>高级：调用方式与账户用量</summary>
                  <label className="field-label"><span>服务 ID</span><input {...fieldProps('id')} value={draft.id} disabled={editing} onChange={event => setDraft(current => ({ ...current, id: event.target.value }))} placeholder={`自动生成：${suggestedServiceId(serviceName())}`} /></label>
                  <p className="ai-service-hint">同一用途的多个模型共用调用方式。仅在供应商接口不兼容时调整。</p>
                  {configuredOperations(modelSetup).filter(op => op !== 'chat' && op !== 'vision.describe').map(op => <label key={op} className="field-label"><span>{AI_SERVICE_CAPABILITY_LABELS[op]}调用方式</span><select value={modelSetup.adapters[op] || ({ 'image.generate': 'openai-images', 'image.edit': 'openai-images-edit', 'video.generate': 'ark-video', 'audio.speech': 'openai-speech', 'audio.transcribe': 'openai-audio' } as Record<string, string>)[op]} onChange={event => setModelSetup(current => ({ ...current, adapters: { ...current.adapters, [op]: event.target.value } }))}>
                    {({ 'image.generate': [['openai-images', 'OpenAI 兼容'], ['ark-images', '火山方舟']], 'image.edit': [['openai-images-edit', 'OpenAI 兼容'], ['ark-images', '火山方舟']], 'video.generate': [['ark-video', '火山方舟'], ['wan-video', '阿里云百炼万相']], 'audio.speech': [['openai-speech', 'OpenAI 兼容'], ['fish-audio-tts', 'Fish Audio']], 'audio.transcribe': [['openai-audio', 'OpenAI 兼容'], ['fish-audio-asr', 'Fish Audio']] } as Record<string, string[][]>)[op].map(([id, label]) => <option key={id} value={id}>{label}</option>)}
                  </select></label>)}
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
                    <label className="field-label ai-service-field"><span>New API User ID</span><input {...fieldProps('balance_user_id')} value={draft.balance_user_id} onChange={(event) => setDraft((current) => ({ ...current, balance_user_id: event.target.value }))} placeholder="用户 ID" /></label>
                  </div> : null}
                  <small className="ai-service-hint">仅手动查询；请求留在服务同源。</small>
                </div>
                </details>
              </> : null}

              <div className="modal-actions">
                <button className="btn" disabled={saving} onClick={formStep === 0 ? closeForm : goBack}>{formStep === 0 ? '取消' : <><ChevronLeft size={14} />上一步</>}</button>
                {formStep === 0 ? <button className="btn btn-primary" disabled={saving} onClick={goNext}>选择模型<ChevronRight size={14} /></button> : <button className="btn btn-primary" disabled={saving} onClick={() => void saveService()}>{saving ? '保存中...' : editing ? '保存修改' : '保存服务'}</button>}
              </div>
            </div>
          </div>
        </ModalSurface>
      ) : null}
    </div>
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
function CustomSourceRow({ service, active, capabilities, routeBadges, health, connectionCheck, clientStatuses, balanceSnapshot, balanceLoading, balanceBusy, balanceError, cloneBusy, onCheckConnection, onRefreshBalance, onClone, onEdit, onRemove }: {
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
  cloneBusy: boolean;
  onCheckConnection: () => void;
  onRefreshBalance: () => void;
  onClone: () => void;
  onEdit: () => void;
  onRemove: () => void;
}) {
  const confirm = useConfirm();
  const canDelete = clientStatuses.length === 0;
  const hasChat = capabilities.includes('chat');
  const summary = `${service.base_url} · ${protocolLabels[service.protocol] ?? service.protocol} · ${hasChat ? `对话模型 ${service.model}` : '已配置模型用途'}`;
  // 一行元信息替代原先两排药丸：能力、默认路由、分发都只是「读一眼」的事实，
  // 药丸的底色会把它们抬成和标题同级的信息，行里就只剩下色块。
  const capabilityText = capabilities.map(capability => AI_SERVICE_CAPABILITY_LABELS[capability]).join(' / ');
  // 分发目标直接写名字，比「已分发 N 个工具」再单起一行列工具名少一行；
  // 名字最多三个，剩下的用 +N，完整清单在 title 里。
  const clientNames = clientStatuses.map(client => clientLabel(client.target));
  const distribution = clientNames.length
    ? `已分发 ${clientNames.slice(0, 3).join('、')}${clientNames.length > 3 ? ` +${clientNames.length - 3}` : ''}`
    : '未分发';
  const facts = [capabilityText, ...routeBadges, distribution].filter(Boolean);
  return (
    <article className={`ai-client-row ai-service-row${active ? ' active' : ''}`}>
      <div className="ai-client-icon target"><PlugZap size={18} /></div>
      <div className="ai-client-copy">
        <strong>{service.display_name}</strong>
        <span title={summary}>{summary}</span>
        <div className="ai-service-fact-line" title={clientNames.length ? `已分发：${clientNames.join('、')}` : undefined}>
          <span className={`ai-service-health is-${health.kind}`}><span className="status-dot" aria-hidden="true" />{health.text}</span>
          {facts.map(fact => <span key={fact}>{fact}</span>)}
          {canDelete ? null : <span className="ai-service-delete-note">有工具在用，不可删除</span>}
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
      </div>
      <div className="ai-client-registration-actions ai-service-row-actions" role="group" aria-label={`${service.display_name} 操作`}>
        <button type="button" className="btn btn-icon ai-service-icon-btn" disabled={connectionCheck?.state === 'busy'} title="检查服务模型目录；不代表全部模型已验证" aria-label={`检查 ${service.display_name}`} onClick={onCheckConnection}>
          {connectionCheck?.state === 'busy' ? <BusyIndicator size={12} /> : <RefreshCw size={14} />}
        </button>
        <button type="button" className="btn btn-icon ai-service-icon-btn" disabled={cloneBusy} title="复制连接、模型、能力和加密凭据，创建独立 AI 服务" aria-label={`复制 AI 服务 ${service.display_name}`} onClick={onClone}>{cloneBusy ? <BusyIndicator size={12} /> : <Copy size={14} />}</button>
        <button type="button" className="btn btn-icon ai-service-icon-btn" title="编辑 AI 服务连接、模型与能力" aria-label={`编辑 AI 服务 ${service.display_name}`} onClick={onEdit}><Pencil size={14} /></button>
        <button type="button" className="btn btn-icon ai-service-icon-btn" disabled={!canDelete} title={canDelete ? '删除这个 AI 服务' : '有 AI 工具正在使用该 AI 服务，请先取消分发'} aria-label={`删除 AI 服务 ${service.display_name}`} onClick={() => { void confirm({ title: `删除 AI 服务「${service.display_name}」？`, description: '删除后需要重新填写连接信息和密钥才能再使用。', confirmText: '删除' }).then((accepted) => { if (accepted) onRemove(); }); }}><Trash2 size={14} /></button>
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
          <strong>工作台 AI 服务</strong>
          <span>{managed.model} · {models} · {managed.base_url}</span>
          <div className="ai-service-fact-line">
            <span className="ai-service-health is-neutral"><span className="status-dot" aria-hidden="true" />工作台</span>
            <span>{boundClients.length ? `已分发 ${boundClients.map(client => clientLabel(client.target)).slice(0, 3).join('、')}` : '未分发'}</span>
          </div>
        </div>
      </article>
    );
  }
  const reason = managed.reason ?? 'unknown';
  const reasonText: Record<string, string> = {
    not_authorized: '工作台 AI 服务未连接',
    user_mismatch: '工作台账号不一致',
    independent: '未连接工作台 AI 服务',
    no_credential: '工作台 AI 服务未配置',
    not_ready: '工作台 AI 服务暂不可用',
    network_error: '工作台连接失败',
    dashboard_error: '工作台 AI 服务读取失败',
    parse_error: '工作台 AI 服务读取失败',
  };
  return (
    <section className="ai-managed-strip">
      <div className="ai-managed-icon muted">{reason === 'not_authorized' ? <CircleDashed size={18} /> : <CircleX size={18} />}</div>
      <div className="ai-managed-copy">
        <strong>{reasonText[reason] ?? '工作台 AI 服务不可用'}</strong>
        <span>{reason === 'not_authorized' || reason === 'user_mismatch' ? '连接账号后即可使用' : reason === 'network_error' || reason === 'dashboard_error' || reason === 'parse_error' ? '请稍后重试' : '请在工作台完成配置'}</span>
      </div>
      <div className="ai-managed-actions">
        {reason === 'not_authorized' || reason === 'user_mismatch' ? <button className="btn" onClick={onOpenAccount}><ExternalLink size={13} />连接账号</button> : null}
        {reason === 'no_credential' || reason === 'not_ready' ? <button className="btn" onClick={onOpenAccount}><ExternalLink size={13} />配置服务</button> : null}
        {reason === 'network_error' || reason === 'dashboard_error' || reason === 'parse_error' ? <button className="btn btn-icon" title="重新读取工作台 AI 服务" aria-label="重新读取工作台 AI 服务" onClick={onRefresh}><RefreshCw size={14} /></button> : null}
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
