import { useState } from 'react';
import { CircleDashed, CircleX, ExternalLink, Pencil, PlugZap, RefreshCw, ShieldCheck, Sparkles, Unplug, X } from 'lucide-react';
import { Pill } from '../components/Common';
import { aiServicePresets } from './aiServicePresets';
import type { AIServiceListResult, CustomAIService, ManagedAIServiceSummary } from '../services/agentApi';

type AiServicesPanelProps = {
  aiServices: AIServiceListResult | null;
  onRefresh: () => void;
  onSaveAIService: (input: {
    id: string;
    display_name: string;
    base_url: string;
    protocol: 'openai-chat' | 'openai-responses';
    model: string;
    models: string[];
    api_key: string;
  }) => Promise<void>;
  onRemoveAIService: (id: string) => Promise<void>;
  onImportAIClient: (target: string, service?: string) => Promise<void>;
  onRemoveAIClient: (target: string) => Promise<void>;
  onOpenAccount: () => void;
  onFetchModels: (input: { base_url: string; api_key: string }) => Promise<string[]>;
  onFetchSavedModels: (id: string, base_url: string) => Promise<string[]>;
};

const emptyDraft = {
  id: '',
  display_name: '',
  base_url: '',
  protocol: 'openai-responses' as 'openai-chat' | 'openai-responses',
  model: '',
  models: '',
  api_key: '',
};

export function AiServicesPanel({
  aiServices,
  onRefresh,
  onSaveAIService,
  onRemoveAIService,
  onImportAIClient,
  onRemoveAIClient,
  onOpenAccount,
  onFetchModels,
  onFetchSavedModels,
}: AiServicesPanelProps) {
  const [formOpen, setFormOpen] = useState(false);
  const [editingServiceId, setEditingServiceId] = useState<string | null>(null);
  const [draft, setDraft] = useState(emptyDraft);
  const [saving, setSaving] = useState(false);
  const [fetchingModels, setFetchingModels] = useState(false);
  const [formError, setFormError] = useState('');
  const [importingClient, setImportingClient] = useState<string | null>(null);
  const [removingClient, setRemovingClient] = useState<string | null>(null);
  const [importService, setImportService] = useState<Record<string, string>>({});
  const [selectedPreset, setSelectedPreset] = useState<string>('');

  const customServices = aiServices?.custom?.services ?? [];
  const managed = aiServices?.managed ?? { available: false };
  const clientStatuses = aiServices?.clients?.targets ?? [];
  const importedClientCount = clientStatuses.filter((client) => client.state === 'imported').length;
  const serviceCount = customServices.length + 1;
  const editing = editingServiceId !== null;

  function applyPreset(presetId: string) {
    const preset = aiServicePresets.find((item) => item.id === presetId);
    if (!preset) return;
    setSelectedPreset(presetId);
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

  const presetGroups = Object.entries(
    aiServicePresets.reduce<Record<string, typeof aiServicePresets>>((groups, preset) => {
      (groups[preset.category] ||= []).push(preset);
      return groups;
    }, {})
  ).map(([name, items]) => ({ name, items }));

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
        ? await onFetchModels({ base_url: draft.base_url.trim(), api_key: draft.api_key.trim() })
        : await onFetchSavedModels(draft.id.trim(), draft.base_url.trim());
      setDraft((current) => ({ ...current, models: models.join(', ') }));
    } catch (error) {
      setFormError(formatAIServiceError(error, '拉取模型失败，请检查 Base URL 和 API Key。'));
    } finally {
      setFetchingModels(false);
    }
  }

  async function importToClient(targetId: string, serviceId: string) {
    setImportingClient(targetId);
    try {
      await onImportAIClient(targetId, `custom:${serviceId}`);
    } finally {
      setImportingClient(null);
      await onRefresh();
    }
  }

  async function removeFromClient(targetId: string) {
    setRemovingClient(targetId);
    try {
      await onRemoveAIClient(targetId);
    } finally {
      setRemovingClient(null);
      await onRefresh();
    }
  }

  function openNewService() {
    setSelectedPreset('');
    setEditingServiceId(null);
    setDraft(emptyDraft);
    setFormError('');
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
      model: service.model,
      models: service.models.join(', '),
      api_key: '',
    });
    setFormError('');
    setFormOpen(true);
  }

  return (
    <div className="ai-services-view">
      <section className="ai-services-summary">
        <div className="ai-services-summary-stats">
          <div><span>AI 服务</span><strong>{serviceCount}</strong></div>
          <div><span>已注册客户端</span><strong>{importedClientCount}</strong></div>
        </div>
        <div className="ai-services-summary-actions">
          <button className="btn btn-primary" onClick={openNewService}>
            新增服务
          </button>
        </div>
      </section>

      {formOpen ? (
        <div className="modal-backdrop" onMouseDown={(event) => { if (event.target === event.currentTarget && !saving) setFormOpen(false); }}>
          <section className="modal ai-service-modal" role="dialog" aria-modal="true" aria-label={editing ? '编辑 AI 服务' : '新增 AI 服务'}>
            <div className="modal-header">
              <div>
                <h3>{editing ? '编辑 AI 服务' : '新增 AI 服务'}</h3>
                <p>{editing ? '更新连接参数；API Key 留空表示继续使用已保存凭据。' : '添加一个模型供应商服务；API Key 加密保存于本机。'}</p>
              </div>
              <button className="btn btn-icon" title="关闭" aria-label="关闭" disabled={saving} onClick={() => setFormOpen(false)}><X size={16} /></button>
            </div>
            <div className="modal-body ai-service-modal-body">
              {formError ? <div className="ai-service-form-error" role="alert"><CircleX size={15} /><span>{formError}</span></div> : null}
              <div className="ai-service-presets">
                <div className="ai-service-group-label">快捷方式</div>
                <div className="ai-service-preset-tabs">
                  <button type="button" className={`ai-service-preset-tab${!selectedPreset ? ' active' : ''}`} onClick={() => { setSelectedPreset(''); setDraft(emptyDraft); }}>
                    <Pencil size={13} />手动配置
                  </button>
                  {presetGroups.map((group) => (
                    <button key={group.name} type="button" className={`ai-service-preset-tab${selectedPreset && group.items.some((item) => item.id === selectedPreset) ? ' active' : ''}`} onClick={() => applyPreset(group.items[0].id)}>
                      <Sparkles size={13} />{group.name}
                    </button>
                  ))}
                </div>
                {selectedPreset ? (
                  <div className="ai-service-preset-list">
                    {presetGroups.flatMap((group) => group.items).map((preset) => (
                      <button key={preset.id} type="button" className={`ai-service-preset-chip${selectedPreset === preset.id ? ' active' : ''}`} title={preset.description} onClick={() => applyPreset(preset.id)}>
                        <strong>{preset.name}</strong><span>{preset.description}</span>
                      </button>
                    ))}
                  </div>
                ) : (
                  <div className="ai-service-preset-hint"><Sparkles size={13} />选择预设将自动填充连接信息与推荐模型，仍可继续调整。</div>
                )}
              </div>

              <div className="ai-service-form-group">
                <div className="ai-service-group-label">连接信息</div>
                <div className="ai-service-form-grid">
                  <label className="field-label ai-service-field"><span>服务 ID</span><input value={draft.id} disabled={editing} onChange={(event) => setDraft((current) => ({ ...current, id: event.target.value }))} placeholder="如 my-gateway" /></label>
                  <label className="field-label ai-service-field"><span>显示名称</span><input value={draft.display_name} onChange={(event) => setDraft((current) => ({ ...current, display_name: event.target.value }))} placeholder="我的网关" /></label>
                  <label className="field-label ai-service-field ai-service-field-wide"><span>Base URL</span><input value={draft.base_url} onChange={(event) => setDraft((current) => ({ ...current, base_url: event.target.value }))} placeholder="https://api.example.com/v1" /></label>
                  <label className="field-label ai-service-field"><span>协议</span><select value={draft.protocol} onChange={(event) => setDraft((current) => ({ ...current, protocol: event.target.value as 'openai-chat' | 'openai-responses' }))}><option value="openai-responses">OpenAI Responses</option><option value="openai-chat">OpenAI Chat</option></select></label>
                </div>
              </div>

              <div className="ai-service-form-group">
                <div className="ai-service-group-label">模型</div>
                <div className="ai-service-form-grid">
                  <label className="field-label ai-service-field"><span>默认模型</span><input value={draft.model} onChange={(event) => setDraft((current) => ({ ...current, model: event.target.value }))} placeholder="如 gpt-test" /></label>
                  <label className="field-label ai-service-field"><span>模型列表</span><input value={draft.models} onChange={(event) => setDraft((current) => ({ ...current, models: event.target.value }))} placeholder="gpt-test, gpt-test-2" /></label>
                </div>
                <button className="btn ai-service-fetch-models" title={!draft.api_key.trim() && !editing ? '请输入 API Key 后再拉取模型' : '请求服务的 /models 接口'} disabled={fetchingModels || !draft.base_url.trim() || (!draft.api_key.trim() && !editing)} onClick={() => void fetchModels()}>
                  <RefreshCw size={14} />{fetchingModels ? '拉取中...' : '从服务拉取模型'}
                </button>
                {!draft.api_key.trim() ? <span className="ai-service-fetch-hint">{editing ? '将使用本机已保存的 API Key 请求模型列表，不会回显或发送到页面。' : '请输入 API Key 后才可拉取模型。'}</span> : null}
              </div>

              <div className="ai-service-form-group">
                <div className="ai-service-group-label">凭据</div>
                <div className="ai-service-form-grid">
                  <label className="field-label ai-service-field ai-service-field-wide"><span>API Key</span><input type="password" value={draft.api_key} onChange={(event) => setDraft((current) => ({ ...current, api_key: event.target.value }))} placeholder={editing ? '留空以保留当前 Key；输入新 Key 可轮换' : 'sk-...'} /></label>
                </div>
              </div>

              <div className="modal-actions">
                <button className="btn" disabled={saving} onClick={() => { setFormOpen(false); setEditingServiceId(null); }}>取消</button>
                <button className="btn btn-primary" disabled={saving || !draft.id.trim() || !draft.display_name.trim() || !draft.base_url.trim() || !draft.model.trim() || (!editing && !draft.api_key.trim())} onClick={() => void saveService()}>
                  {saving ? '保存中...' : editing ? '保存修改' : '保存服务'}
                </button>
              </div>
            </div>
          </section>
        </div>
      ) : null}

      <section className="ai-services-section">
        <div className="ai-section-heading ai-services-heading">
          <div><h3>AI 服务</h3><span>选择服务并注册到相关 AI 客户端；Dashboard 分发服务和本机服务统一展示。</span></div>
          <Pill kind="neutral">{serviceCount}</Pill>
        </div>

        {serviceCount ? (
          <div className="ai-client-list">
            <ManagedServiceCard managed={managed} clientStatuses={clientStatuses} importing={importingClient} removing={removingClient} onImport={async (target) => { setImportingClient(target); try { await onImportAIClient(target, 'managed'); } finally { setImportingClient(null); await onRefresh(); } }} onRemove={removeFromClient} onOpenAccount={onOpenAccount} onRefresh={onRefresh} />
            {customServices.map((service) => (
              <AiServiceRow key={service.id} service={service} clientStatuses={clientStatuses} importing={importingClient} removing={removingClient} importSelection={importService[service.id] ?? ''} onImportSelectionChange={(targetId) => setImportService((current) => ({ ...current, [service.id]: targetId }))} onImport={importToClient} onEdit={openEditService} onRemove={(id) => void onRemoveAIService(id).then(onRefresh)} />
            ))}
          </div>
        ) : (
          <div className="ai-empty-row"><span className="ai-empty-icon"><CircleDashed size={14} /></span><span>暂无可用 AI 服务，点击「新增服务」添加本机服务，或先配置 Dashboard 分发服务。</span></div>
        )}
      </section>
    </div>
  );
}

const clientLabels: Record<string, string> = {
  vscode: 'VS Code',
  'cc-switch': 'CC Switch',
  codex: 'Codex',
  workbuddy: 'WorkBuddy',
  'kimi-code': 'Kimi Code',
  'qwen-code': 'Qwen Code',
  'claude-code': 'Claude Code',
  'claude-desktop': 'Claude Desktop',
};

function AiServiceRow({ service, clientStatuses, importing, removing, importSelection, onImportSelectionChange, onImport, onEdit, onRemove }: {
  service: CustomAIService;
  clientStatuses: { target: string; state: string; client_detected: boolean; detail: string; service?: string }[];
  importing: string | null;
  removing: string | null;
  importSelection: string;
  onImportSelectionChange: (targetId: string) => void;
  onImport: (targetId: string, serviceId: string) => void;
  onEdit: (service: CustomAIService) => void;
  onRemove: (id: string) => void;
}) {
  const source = `custom:${service.id}`;
  const boundClients = clientStatuses.filter((client) => client.state === 'imported' && client.service === source);
  const unknownImported = clientStatuses.some((client) => client.state === 'imported' && !client.service);
  const selectableClients = clientStatuses.filter((client) => client.state !== 'imported' || client.service === source);
  const selectedClient = clientStatuses.find((client) => client.target === importSelection);
  const canDelete = boundClients.length === 0 && !unknownImported;
  return (
    <article className="ai-client-row">
      <div className="ai-client-icon target"><PlugZap size={18} /></div>
      <div className="ai-client-copy">
        <strong>{service.display_name}</strong>
        <span>{service.base_url} · {service.protocol === 'openai-responses' ? 'Responses' : 'Chat'} · 模型 {service.model}{service.models.length > 1 ? ` 等 ${service.models.length} 个` : ''}{boundClients.length ? ` · 已注册 ${boundClients.length} 个客户端，修改后请同步` : ''}</span>
      </div>
      <Pill kind="neutral">{service.models.length ? `${service.models.length} 模型` : '未配置模型'}</Pill>
      <div className="ai-client-registration-actions">
        <select
          aria-label={`选择注册 ${service.display_name} 的客户端`}
          value={importSelection}
          disabled={!selectableClients.length || importing !== null || removing !== null}
          onChange={(event) => onImportSelectionChange(event.target.value)}
        >
          <option value="">{selectableClients.length ? '选择客户端' : '暂无可用客户端'}</option>
          {clientStatuses.map((client) => <option key={client.target} value={client.target} disabled={client.state === 'imported' && client.service !== source}>{clientLabels[client.target] ?? client.target}{client.state === 'imported' ? (client.service === source ? ' · 已注册，可同步' : ' · 已注册其他服务') : ''}</option>)}
        </select>
        <button
          className="btn btn-icon btn-primary"
          title={selectedClient?.service === source ? `同步 ${service.display_name} 到客户端` : `将 ${service.display_name} 注册到客户端`}
          aria-label={selectedClient?.service === source ? `同步 ${service.display_name} 到客户端` : `将 ${service.display_name} 注册到客户端`}
          disabled={importing !== null || !importSelection}
          onClick={() => onImport(importSelection, service.id)}
        >
          {selectedClient?.service === source ? <RefreshCw size={15} /> : <PlugZap size={15} />}
        </button>
        <button className="btn btn-icon" title={`编辑 ${service.display_name}`} aria-label={`编辑 ${service.display_name}`} onClick={() => onEdit(service)}><Pencil size={15} /></button>
        <button className="btn btn-icon ai-row-remove" title={!canDelete ? '请先取消注册此服务关联的客户端' : `删除 ${service.display_name}`} aria-label={!canDelete ? '请先取消注册此服务关联的客户端' : `删除 ${service.display_name}`} disabled={!canDelete || removing !== null} onClick={() => { if (window.confirm(`确认删除 AI 服务“${service.display_name}”？`)) onRemove(service.id); }}><Unplug size={15} /></button>
      </div>
    </article>
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

function ManagedServiceCard({ managed, clientStatuses, importing, removing, onImport, onRemove, onOpenAccount, onRefresh }: { managed: ManagedAIServiceSummary; clientStatuses: { target: string; state: string; service?: string }[]; importing: string | null; removing: string | null; onImport: (target: string) => Promise<void>; onRemove: (target: string) => Promise<void>; onOpenAccount: () => void; onRefresh: () => void }) {
  const [selection, setSelection] = useState('');
  if (managed.available) {
    const models = managed.models?.length ? `${managed.models.length} 个模型` : '未返回模型列表';
    const source = 'managed';
    const boundClients = clientStatuses.filter((client) => client.state === 'imported' && client.service === source);
    const selectableClients = clientStatuses.filter((client) => client.state !== 'imported' || client.service === source);
    const selected = clientStatuses.find((client) => client.target === selection);
    return (
      <section className="ai-managed-strip ready">
        <div className="ai-managed-icon"><ShieldCheck size={18} /></div>
        <div className="ai-managed-copy">
          <strong>Dashboard 分发服务已就绪</strong>
          <span>{managed.model} · {models} · {managed.base_url}{boundClients.length ? ` · 已注册 ${boundClients.length} 个客户端` : ''}</span>
        </div>
        <Pill kind="success">已接入</Pill>
        <div className="ai-managed-actions">
          <select aria-label="选择注册 Dashboard 分发服务的客户端" value={selection} disabled={!selectableClients.length || importing !== null || removing !== null} onChange={(event) => setSelection(event.target.value)}>
            <option value="">{selectableClients.length ? '选择客户端' : '暂无可用客户端'}</option>
            {clientStatuses.map((client) => <option key={client.target} value={client.target} disabled={client.state === 'imported' && client.service !== source}>{clientLabels[client.target] ?? client.target}{client.state === 'imported' ? (client.service === source ? ' · 已注册，可同步' : ' · 已注册其他服务') : ''}</option>)}
          </select>
          <button className="btn btn-icon btn-primary" title={selected?.service === source ? '同步 Dashboard 分发服务' : '注册 Dashboard 分发服务'} aria-label={selected?.service === source ? '同步 Dashboard 分发服务' : '注册 Dashboard 分发服务'} disabled={!selection || importing !== null} onClick={() => void onImport(selection)}>{selected?.service === source ? <RefreshCw size={14} /> : <PlugZap size={14} />}</button>
          {boundClients.length ? <button className="btn btn-icon ai-row-remove" title="取消 Dashboard 分发服务注册" aria-label="取消 Dashboard 分发服务注册" disabled={removing !== null} onClick={() => { const target = selection || boundClients[0].target; if (window.confirm(`确认取消 ${target} 的 Dashboard 分发服务注册？`)) void onRemove(target); }}><Unplug size={14} /></button> : null}
        </div>
      </section>
    );
  }
  const reason = managed.reason ?? 'unknown';
  const reasonText: Record<string, string> = {
    not_authorized: '尚未连接工作台账号，连接后可自动使用 Dashboard 分发的 AI 服务',
    user_mismatch: '本机 Agent 授权账号与当前 Dashboard 用户不一致，请重新授权',
    independent: '当前为独立模式，没有 Dashboard 分发的 AI 服务',
    no_credential: 'Dashboard 尚未给当前账号生成 AI 凭证，请先在「我的接入」中选择渠道',
    not_ready: '当前 AI 凭证未处于可用状态，请先在 Dashboard 选择有效渠道',
    network_error: '无法连接 Dashboard，稍后自动重试',
    dashboard_error: 'Dashboard 读取 AI 接入失败，请稍后重试',
    parse_error: 'Dashboard AI 服务信息暂时无法读取，请刷新后重试',
  };
  return (
    <section className="ai-managed-strip">
      <div className="ai-managed-icon muted">{reason === 'not_authorized' ? <CircleDashed size={18} /> : <CircleX size={18} />}</div>
      <div className="ai-managed-copy">
        <strong>Dashboard 分发服务未就绪</strong>
        <span>{reasonText[reason] ?? 'Dashboard 暂未提供可用的 AI 服务'}</span>
      </div>
      <div className="ai-managed-actions">
        {reason === 'not_authorized' || reason === 'user_mismatch' ? <button className="btn" onClick={onOpenAccount}><ExternalLink size={13} />连接账号</button> : null}
        {reason === 'no_credential' || reason === 'not_ready' ? <button className="btn" onClick={onOpenAccount}><ExternalLink size={13} />配置接入</button> : null}
        {reason === 'network_error' || reason === 'dashboard_error' || reason === 'parse_error' ? <button className="btn btn-icon" title="重试读取 Dashboard 服务" aria-label="重试读取 Dashboard 服务" onClick={onRefresh}><RefreshCw size={14} /></button> : null}
        <Pill kind="neutral">未接入</Pill>
      </div>
    </section>
  );
}
