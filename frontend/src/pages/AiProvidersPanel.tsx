import { useEffect, useRef, useState } from 'react';
import { ChevronDown, CircleDashed, CircleX, ExternalLink, Pencil, PlugZap, RefreshCw, ShieldCheck, Sparkles, Trash2, X } from 'lucide-react';
import { Pill } from '../components/Common';
import { BusyIndicator } from '../components/BusyIndicator';
import { useConfirm } from '../components/ConfirmDialog';
import { fallbackAiServicePresets, type AiServicePreset } from './aiServicePresets';
import { clientLabel } from '../utils/clientLabels';
import type { AIProviderImportStatus, AIServiceListResult, AIServiceProtocol, AIServiceTemplateListResult, CustomAIService, ManagedAIServiceSummary } from '../services/agentApi';

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
    api_key: string;
  }) => Promise<void>;
  onSetActiveAIService: (id: string) => Promise<void>;
  onRemoveAIService: (id: string) => Promise<void>;
  onOpenAccount: () => void;
  onFetchModels: (input: { base_url: string; api_key: string; protocol: AIServiceProtocol }) => Promise<string[]>;
  onFetchSavedModels: (id: string, base_url: string) => Promise<string[]>;
};

const emptyDraft = {
  id: '',
  display_name: '',
  base_url: '',
  protocol: 'openai-responses' as AIServiceProtocol,
  model: '',
  models: '',
  api_key: '',
};

export function AiProvidersPanel({
  aiServices,
  templates,
  onRefresh,
  onSaveAIService,
  onSetActiveAIService,
  onRemoveAIService,
  onOpenAccount,
  onFetchModels,
  onFetchSavedModels,
}: AiProvidersPanelProps) {
  const [formOpen, setFormOpen] = useState(false);
  const [editingServiceId, setEditingServiceId] = useState<string | null>(null);
  const [draft, setDraft] = useState(emptyDraft);
  const [saving, setSaving] = useState(false);
  const [fetchingModels, setFetchingModels] = useState(false);
  const [formError, setFormError] = useState('');
  const [settingActive, setSettingActive] = useState(false);
  const [selectedPreset, setSelectedPreset] = useState<string>('');
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const modalRef = useRef<HTMLElement | null>(null);
  const restoreFocusRef = useRef<HTMLElement | null>(null);

  const customServices = aiServices?.custom?.services ?? [];
  const activeServiceId = aiServices?.custom?.active_service_id ?? '';
  const managed = aiServices?.managed ?? { available: false };
  const independentMode = managed.reason === 'independent';
  const clientStatuses = aiServices?.clients?.targets ?? [];
  const serviceCount = customServices.length + (independentMode || !managed.available ? 0 : 1);
  const editing = editingServiceId !== null;

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

  const chatRouteValue = customServices.some((service) => service.id === activeServiceId) ? activeServiceId : '';
  const chatRouteDefaultLabel = independentMode
    ? '本机默认配置'
    : managed.available ? '工作台模型服务' : '工作台模型服务（未就绪）';

  async function changeChatRoute(id: string) {
    if (id === chatRouteValue || settingActive) return;
    setSettingActive(true);
    try {
      await onSetActiveAIService(id);
    } finally {
      setSettingActive(false);
    }
  }

  const workbenchPresets = templates?.items ?? [];
  const usingFallbackPresets = workbenchPresets.length === 0;
  const presetEntries: AiServicePreset[] = usingFallbackPresets ? fallbackAiServicePresets : workbenchPresets;

  function applyPreset(presetId: string) {
    const preset = presetEntries.find((item) => item.id === presetId);
    if (!preset) return;
    setSelectedPreset(presetId);
    setAdvancedOpen(false);
    setFormError('');
    setDraft({
      id: preset.id,
      display_name: preset.name,
      base_url: preset.base_url,
      protocol: preset.protocol,
      model: preset.default_model,
      models: preset.models.join(', '),
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
  const activePresetGroup = presetGroups.find((group) => group.items.some((item) => item.id === selectedPreset));

  async function saveService() {
    if (!draft.id.trim() || !draft.display_name.trim() || !draft.base_url.trim() || !draft.model.trim() || (!editing && !draft.api_key.trim())) return;
    setFormError('');
    setSaving(true);
    try {
      await onSaveAIService({
        id: draft.id.trim(),
        display_name: draft.display_name.trim(),
        base_url: draft.base_url.trim(),
        protocol: draft.protocol,
        model: draft.model.trim(),
        models: draft.models.split(/[\n,]/).map((item) => item.trim()).filter(Boolean),
        api_key: draft.api_key.trim(),
      });
      setDraft(emptyDraft);
      setEditingServiceId(null);
      setSelectedPreset('');
      setFormOpen(false);
    } catch (error) {
      setFormError(formatAIServiceError(error, '保存服务失败，请检查连接信息后重试。'));
    } finally {
      setSaving(false);
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

  function openNewService() {
    restoreFocusRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setSelectedPreset('');
    setEditingServiceId(null);
    setDraft(emptyDraft);
    setFormError('');
    setAdvancedOpen(false);
    setFormOpen(true);
  }

  function openEditService(service: CustomAIService) {
    restoreFocusRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setSelectedPreset('');
    setEditingServiceId(service.id);
    setDraft({
      id: service.id,
      display_name: service.display_name,
      base_url: service.base_url,
      protocol: service.protocol,
      model: service.model,
      models: service.models.join(', '),
      api_key: '',
    });
    setFormError('');
    setAdvancedOpen(true);
    setFormOpen(true);
  }

  function closeForm() {
    if (saving) return;
    setFormOpen(false);
    setEditingServiceId(null);
  }

  return (
    <div className="ai-providers-view">
      <section className={`ai-overview ${serviceCount ? 'ready' : 'attention'}`}>
        <div className="ai-overview-main">
          <div className="ai-overview-icon"><PlugZap size={20} /></div>
          <div className="ai-overview-copy">
            <span className="ai-overview-eyebrow">模型来源</span>
            <strong>{serviceCount ? `${serviceCount} 个可用来源` : '还没有可用的模型来源'}</strong>
            <span>HiMind 对话和本机工具都从这里取模型。</span>
          </div>
        </div>
        <div className="ai-overview-stats" aria-label="模型来源统计">
          <div title="本机可用的模型来源数量（含工作台服务）"><span>来源</span><strong>{serviceCount}</strong></div>
          <div title="自行填写的本机模型服务数量"><span>自定义</span><strong>{customServices.length}</strong></div>
        </div>
        <div className="ai-overview-actions">
          <button className="btn btn-primary" onClick={openNewService}>新增来源</button>
        </div>
      </section>

      {serviceCount ? (
        <div className="ai-service-route" role="group" aria-label="HiMind AI 对话使用的模型来源">
          <span className="ai-service-route-label">HiMind AI 对话使用</span>
          <select className="ai-service-route-select" aria-label="HiMind AI 对话使用的模型来源" value={chatRouteValue} disabled={settingActive} onChange={(event) => void changeChatRoute(event.target.value)}>
            <option value="">{chatRouteDefaultLabel}</option>
            {customServices.map((service) => <option key={service.id} value={service.id}>{service.display_name}</option>)}
          </select>
          {settingActive ? <BusyIndicator size={13} /> : null}
          <span className="ai-service-route-hint">只影响 HiMind 自己的对话，分发到工具在「工具接入」里做。</span>
        </div>
      ) : null}

      <section className="ai-client-section">
        <div className="ai-section-heading">
          <div><h3>全部来源</h3><span>来源可编辑、删除；删除前需先取消分发到工具的引用。</span></div>
          <div className="ai-section-tail"><Pill kind="neutral">{customServices.length}</Pill></div>
        </div>

        <div className="ai-client-list">
          {!independentMode ? (
            <ManagedSourceRow managed={managed} active={!chatRouteValue} clientStatuses={clientStatuses} onOpenAccount={onOpenAccount} onRefresh={onRefresh} />
          ) : null}
          {customServices.map((service) => (
            <CustomSourceRow
              key={service.id}
              service={service}
              active={chatRouteValue === service.id}
              clientStatuses={clientStatuses}
              onEdit={() => openEditService(service)}
              onRemove={() => void onRemoveAIService(service.id)}
            />
          ))}
          {!serviceCount ? <div className="ai-empty-row"><span className="ai-empty-icon"><CircleDashed size={14} /></span><span>{independentMode ? '暂无模型来源，点「新增来源」添加本机服务。' : '暂无模型来源，点「新增来源」添加本机服务，或先对接工作台。'}</span></div> : null}
        </div>
      </section>

      {formOpen ? (
        <div className="modal-backdrop" onMouseDown={(event) => { if (event.target === event.currentTarget && !saving) setFormOpen(false); }}>
          <section ref={modalRef} className="modal ai-service-modal" role="dialog" aria-modal="true" aria-labelledby="ai-service-modal-title">
            <div className="modal-header">
              <div>
                <h3 id="ai-service-modal-title">{editing ? '编辑模型来源' : '新增模型来源'}</h3>
                <p>{editing ? 'API Key 留空则保留当前凭据。' : '凭据仅保存在本机。'}</p>
              </div>
              <button className="btn btn-icon" title="关闭" aria-label="关闭" disabled={saving} onClick={closeForm}><X size={16} /></button>
            </div>
            <div className="modal-body ai-service-modal-body">
              {formError ? <div className="ai-service-form-error" role="alert"><CircleX size={15} /><span>{formError}</span></div> : null}
              <div className="ai-service-presets">
                <div className="ai-service-presets-head">
                  <div className="ai-service-group-label">常用服务</div>
                  {usingFallbackPresets ? <span className="ai-service-preset-source" title="连接 AI 工作台后会改用工作台目录里的服务与模型">未连接 AI 工作台，使用内置预设</span> : null}
                </div>
                <div className="ai-service-preset-tabs">
                  <button type="button" className={`ai-service-preset-tab${!selectedPreset ? ' active' : ''}`} onClick={() => { setSelectedPreset(''); setDraft(emptyDraft); setAdvancedOpen(true); }}>
                    <Pencil size={13} />手动配置
                  </button>
                  {presetGroups.map((group) => (
                    <button key={group.name} type="button" className={`ai-service-preset-tab${selectedPreset && group.items.some((item) => item.id === selectedPreset) ? ' active' : ''}`} onClick={() => applyPreset(group.items[0].id)}>
                      <Sparkles size={13} />{group.name}
                    </button>
                  ))}
                </div>
                {activePresetGroup ? (
                  <div className="ai-service-preset-list">
                    {activePresetGroup.items.map((preset) => (
                      <button key={preset.id} type="button" className={`ai-service-preset-chip${selectedPreset === preset.id ? ' active' : ''}`} title={preset.description} onClick={() => applyPreset(preset.id)}>
                        <strong>{preset.name}</strong><span>{preset.description}</span>
                      </button>
                    ))}
                  </div>
                ) : (
                  <div className="ai-service-preset-hint"><Sparkles size={13} />选择常用服务后会自动填入连接信息和推荐模型，可继续修改。</div>
                )}
              </div>

              <div className="ai-service-form-group">
                <div className="ai-service-group-label">服务</div>
                <div className="ai-service-form-grid">
                  <label className="field-label ai-service-field ai-service-field-wide"><span>服务名称</span><input value={draft.display_name} onChange={(event) => setDraft((current) => ({ ...current, display_name: event.target.value }))} placeholder="例如：公司模型" /></label>
                </div>
              </div>

              <div className="ai-service-form-group">
                <div className="ai-service-group-label">模型</div>
                <div className="ai-service-form-grid">
                  <label className="field-label ai-service-field ai-service-field-wide"><span>默认模型</span><input value={draft.model} onChange={(event) => setDraft((current) => ({ ...current, model: event.target.value }))} placeholder="如 gpt-test" /></label>
                </div>
              </div>

              <div className="ai-service-form-group">
                <div className="ai-service-group-label">凭据</div>
                <div className="ai-service-form-grid">
                  <label className="field-label ai-service-field ai-service-field-wide"><span>API Key</span><input type="password" value={draft.api_key} onChange={(event) => setDraft((current) => ({ ...current, api_key: event.target.value }))} placeholder={editing ? '留空以保留当前 Key' : 'sk-...'} /></label>
                </div>
              </div>

              <div className="ai-service-advanced">
                <button type="button" className="ai-service-advanced-toggle" aria-expanded={advancedOpen} onClick={() => setAdvancedOpen((value) => !value)}>
                  <span>高级连接设置</span><ChevronDown size={14} className={advancedOpen ? 'open' : ''} />
                </button>
                {advancedOpen ? <div className="ai-service-advanced-body">
                  <div className="ai-service-form-group">
                    <div className="ai-service-group-label">连接</div>
                    <div className="ai-service-form-grid">
                      <label className="field-label ai-service-field"><span>服务 ID</span><input value={draft.id} disabled={editing} onChange={(event) => setDraft((current) => ({ ...current, id: event.target.value }))} placeholder="如 my-gateway" /></label>
                      <label className="field-label ai-service-field"><span>协议</span><select value={draft.protocol} onChange={(event) => setDraft((current) => ({ ...current, protocol: event.target.value as AIServiceProtocol }))}>{protocolOptions.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}</select></label>
                      <label className="field-label ai-service-field ai-service-field-wide"><span>Base URL</span><input value={draft.base_url} onChange={(event) => setDraft((current) => ({ ...current, base_url: event.target.value }))} placeholder="https://api.example.com/v1" /></label>
                    </div>
                  </div>
                  <div className="ai-service-form-group">
                    <div className="ai-service-group-label">模型列表</div>
                    <div className="ai-service-form-grid">
                      <label className="field-label ai-service-field ai-service-field-wide"><span>可用模型</span><input value={draft.models} onChange={(event) => setDraft((current) => ({ ...current, models: event.target.value }))} placeholder="多个模型用逗号分隔" /></label>
                    </div>
                    <button className="btn ai-service-fetch-models" title={!draft.api_key.trim() && !editing ? '请输入 API Key 后再获取模型' : '获取服务提供的模型列表'} disabled={fetchingModels || !draft.base_url.trim() || (!draft.api_key.trim() && !editing)} onClick={() => void fetchModels()}>
                      {fetchingModels ? <BusyIndicator size={14} /> : <RefreshCw size={14} />}{fetchingModels ? '获取中' : '同步模型'}
                    </button>
                    {!draft.api_key.trim() ? <span className="ai-service-fetch-hint">{editing ? '使用已保存的 API Key，同步时不会显示 Key。' : '填写 API Key 后可同步模型。'}</span> : null}
                  </div>
                </div> : null}
              </div>

              <div className="modal-actions">
                <button className="btn" disabled={saving} onClick={closeForm}>取消</button>
                <button className="btn btn-primary" disabled={saving || !draft.id.trim() || !draft.display_name.trim() || !draft.base_url.trim() || !draft.model.trim() || (!editing && !draft.api_key.trim())} onClick={() => void saveService()}>
                  {saving ? '保存中...' : editing ? '保存修改' : '保存服务'}
                </button>
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

function CustomSourceRow({ service, active, clientStatuses, onEdit, onRemove }: {
  service: CustomAIService;
  active: boolean;
  clientStatuses: AIProviderImportStatus[];
  onEdit: () => void;
  onRemove: () => void;
}) {
  const confirm = useConfirm();
  const source = `custom:${service.id}`;
  const boundClients = clientStatuses.filter((client) => client.state === 'imported' && client.service === source);
  const canDelete = boundClients.length === 0;
  const summary = `${service.base_url} · ${protocolLabels[service.protocol] ?? service.protocol} · 模型 ${service.model}`;
  return (
    <article className={`ai-client-row ai-service-row${active ? ' active' : ''}`}>
      <div className="ai-client-icon target"><PlugZap size={18} /></div>
      <div className="ai-client-copy">
        <strong>{service.display_name}</strong>
        <span title={summary}>{summary}</span>
        <SourceUsageChips clients={boundClients} />
      </div>
      <Pill kind={boundClients.length ? 'success' : 'neutral'}>{boundClients.length ? `已分发 ${boundClients.length} 个` : '未分发'}</Pill>
      <div className="ai-client-registration-actions">
        <button type="button" className="btn ai-service-tool-btn" onClick={onEdit}><Pencil size={14} />编辑</button>
        <button type="button" className="btn ai-service-tool-btn" disabled={!canDelete} title={canDelete ? undefined : '有 AI 工具正在使用该来源，请先取消分发'} onClick={() => { void confirm({ title: `删除模型来源「${service.display_name}」？`, description: '删除后需要重新填写接口地址和密钥才能再使用。', confirmText: '删除' }).then((accepted) => { if (accepted) onRemove(); }); }}><Trash2 size={14} />删除</button>
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
