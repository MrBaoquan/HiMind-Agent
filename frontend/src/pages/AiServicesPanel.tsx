import { useState } from 'react';
import { ChevronDown, CircleDashed, CircleX, ExternalLink, MoreHorizontal, Pencil, PlugZap, RefreshCw, ShieldCheck, Sparkles, Trash2, X } from 'lucide-react';
import { Pill } from '../components/Common';
import { ActionMenu, ActionMenuItem } from '../components/ActionMenu';
import { BusyIndicator } from '../components/BusyIndicator';
import { useConfirm } from '../components/ConfirmDialog';
import { fallbackAiServicePresets, type AiServicePreset } from './aiServicePresets';
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

type AiServicesPanelProps = {
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
  onImportAIClient: (target: string, service?: string, replace?: boolean) => Promise<void>;
  onRemoveAIClient: (target: string) => Promise<void>;
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

/**
 * 正在写入本机 AI 工具配置的目标。分发/取消分发一次只动一个工具，
 * 所以这里按「目标」而不是「整个页面」记忙闲：一个工具在写配置时，
 * 其它工具行仍然可以操作，出错也只影响这一行的反馈。
 */
type ClientPending = { target: string; action: 'import' | 'remove' };

export function AiServicesPanel({
  aiServices,
  templates,
  onRefresh,
  onSaveAIService,
  onSetActiveAIService,
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
  const [pendingClient, setPendingClient] = useState<ClientPending | null>(null);
  const [settingActive, setSettingActive] = useState(false);
  const [selectedPreset, setSelectedPreset] = useState<string>('');

  const customServices = aiServices?.custom?.services ?? [];
  const activeServiceId = aiServices?.custom?.active_service_id ?? '';
  const managed = aiServices?.managed ?? { available: false };
  const independentMode = managed.reason === 'independent';
  const clientStatuses = aiServices?.clients?.targets ?? [];
  const importedClientCount = clientStatuses.filter((client) => client.state === 'imported').length;
  const pendingClientCount = clientStatuses.filter((client) => client.state !== 'imported' && client.client_detected).length;
  const serviceCount = customServices.length + (independentMode ? 0 : 1);
  const editing = editingServiceId !== null;
  // 服务来源 → 展示名，用于在展开区说明某个工具当前用的是哪个服务。
  const serviceNames: Record<string, string> = { managed: '工作台模型服务' };
  for (const service of customServices) serviceNames[`custom:${service.id}`] = service.display_name;

  // 「HiMind AI 对话用哪个服务」是页面级单选（同一时刻只有一个），所以放在列表
  // 上方做成一个设置项，而不是藏在每行的行尾菜单里——后端 active_service_id 也是
  // 这个粒度。选项里的空值就是「不用自定义服务」，也就是工作台服务或本机配置。
  const chatRouteValue = customServices.some((service) => service.id === activeServiceId) ? activeServiceId : '';
  const chatRouteDefaultLabel = independentMode
    ? '本机默认配置'
    : managed.available ? '工作台模型服务' : '工作台模型服务（未就绪）';

  async function changeChatRoute(id: string) {
    if (id === chatRouteValue || settingActive) return;
    setSettingActive(true);
    try {
      // 切换动作自身已经回读了服务列表（调用方完成写入后就刷新），
      // 这里不再补一次整页刷新：多出来的那轮 MCP 目标探测只会让下拉卡住两秒。
      await onSetActiveAIService(id);
    } finally {
      setSettingActive(false);
    }
  }

  // 预设来自工作台 AI 服务目录；独立模式或目录暂不可用时用内置兜底列表。
  const workbenchPresets = templates?.items ?? [];
  const usingFallbackPresets = workbenchPresets.length === 0;
  const presetEntries: AiServicePreset[] = usingFallbackPresets ? fallbackAiServicePresets : workbenchPresets;

  function applyPreset(presetId: string) {
    const preset = presetEntries.find((item) => item.id === presetId);
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

  async function importToClients(targets: string[], service?: string, replace = false) {
    const list = [...new Set(targets.filter(Boolean))];
    if (!list.length) return;
    setPendingClient({ target: list[0], action: 'import' });
    try {
      for (const target of list) await onImportAIClient(target, service, replace);
    } finally {
      // 写入结果由 onImportAIClient 里那次 refreshAIServices 带回页面，
      // 这里不再补一次整页刷新：多出来的那轮读取只会让按钮多转一秒。
      setPendingClient(null);
    }
  }

  async function removeFromClient(targetId: string) {
    setPendingClient({ target: targetId, action: 'remove' });
    try {
      await onRemoveAIClient(targetId);
    } finally {
      setPendingClient(null);
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
          {/* 三个数必须能被用户自己核对：可用的服务数、已经分发出去的工具数、
              还没分发的工具数。原先把「已连接 AI 工具」按客户端总数统计，和每个
              服务行里的数字对不上，所以这里按「工具」口径统一。 */}
          <div title="本机可用的模型服务数量（含工作台服务）"><span>模型服务</span><strong>{serviceCount}</strong></div>
          <div title="已分发到某个模型服务的本机 AI 工具数量"><span>已分发工具</span><strong>{importedClientCount}</strong></div>
          <div title="本机已检测到、但还没有分发任何模型服务的 AI 工具数量"><span>待分发工具</span><strong className={pendingClientCount ? 'warning-text' : ''}>{pendingClientCount}</strong></div>
        </div>
        <div className="ai-services-summary-actions">
          <button className="btn btn-primary" onClick={openNewService}>
            新增服务
          </button>
        </div>
      </section>

      {formOpen ? (
        <div className="modal-backdrop" onMouseDown={(event) => { if (event.target === event.currentTarget && !saving) setFormOpen(false); }}>
          <section className="modal ai-service-modal" role="dialog" aria-modal="true" aria-label={editing ? '编辑模型服务' : '新增模型服务'}>
            <div className="modal-header">
              <div>
                <h3>{editing ? '编辑模型服务' : '新增模型服务'}</h3>
                <p>{editing ? '更新连接信息；API Key 留空表示继续使用已保存凭据。' : '添加模型服务；API Key 会加密保存在本机。'}</p>
              </div>
              <button className="btn btn-icon" title="关闭" aria-label="关闭" disabled={saving} onClick={() => setFormOpen(false)}><X size={16} /></button>
            </div>
            <div className="modal-body ai-service-modal-body">
              {formError ? <div className="ai-service-form-error" role="alert"><CircleX size={15} /><span>{formError}</span></div> : null}
              <div className="ai-service-presets">
                <div className="ai-service-presets-head">
                  <div className="ai-service-group-label">常用服务</div>
                  {usingFallbackPresets ? <span className="ai-service-preset-source" title="连接 AI 工作台后会改用工作台目录里的服务与模型">未连接 AI 工作台，使用内置预设</span> : null}
                </div>
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
                <div className="ai-service-group-label">连接信息</div>
                <div className="ai-service-form-grid">
                  <label className="field-label ai-service-field"><span>服务 ID</span><input value={draft.id} disabled={editing} onChange={(event) => setDraft((current) => ({ ...current, id: event.target.value }))} placeholder="如 my-gateway" /></label>
                  <label className="field-label ai-service-field"><span>显示名称</span><input value={draft.display_name} onChange={(event) => setDraft((current) => ({ ...current, display_name: event.target.value }))} placeholder="我的网关" /></label>
                  <label className="field-label ai-service-field ai-service-field-wide"><span>Base URL</span><input value={draft.base_url} onChange={(event) => setDraft((current) => ({ ...current, base_url: event.target.value }))} placeholder="https://api.example.com/v1" /></label>
                  <label className="field-label ai-service-field"><span>协议</span><select value={draft.protocol} onChange={(event) => setDraft((current) => ({ ...current, protocol: event.target.value as AIServiceProtocol }))}>{protocolOptions.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}</select></label>
                </div>
              </div>

              <div className="ai-service-form-group">
                <div className="ai-service-group-label">模型</div>
                <div className="ai-service-form-grid">
                  <label className="field-label ai-service-field"><span>默认模型</span><input value={draft.model} onChange={(event) => setDraft((current) => ({ ...current, model: event.target.value }))} placeholder="如 gpt-test" /></label>
                  <label className="field-label ai-service-field"><span>模型列表</span><input value={draft.models} onChange={(event) => setDraft((current) => ({ ...current, models: event.target.value }))} placeholder="gpt-test, gpt-test-2" /></label>
                </div>
                <button className="btn ai-service-fetch-models" title={!draft.api_key.trim() && !editing ? '请输入 API Key 后再获取模型' : '获取服务提供的模型列表'} disabled={fetchingModels || !draft.base_url.trim() || (!draft.api_key.trim() && !editing)} onClick={() => void fetchModels()}>
                  {fetchingModels ? <BusyIndicator size={14} /> : <RefreshCw size={14} />}{fetchingModels ? '获取中' : '获取模型列表'}
                </button>
                {!draft.api_key.trim() ? <span className="ai-service-fetch-hint">{editing ? '会使用本机已保存的 API Key 获取模型列表，Key 不会显示在页面上。' : '请输入 API Key 后才可获取模型。'}</span> : null}
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
          <div><h3>模型服务</h3><span>可同时分发给多个 AI 工具</span></div>
          <Pill kind="neutral">{serviceCount}</Pill>
        </div>

        {serviceCount ? (
          <div className="ai-service-route">
            <span className="ai-service-route-label">HiMind AI 对话使用</span>
            <select
              className="ai-service-route-select"
              aria-label="HiMind AI 对话使用的模型服务"
              value={chatRouteValue}
              disabled={settingActive}
              onChange={(event) => void changeChatRoute(event.target.value)}
            >
              <option value="">{chatRouteDefaultLabel}</option>
              {customServices.map((service) => <option key={service.id} value={service.id}>{service.display_name}</option>)}
            </select>
            {settingActive ? <BusyIndicator size={13} /> : null}
            <span className="ai-service-route-hint">只影响 HiMind AI 对话，不改动已分发的工具。</span>
          </div>
        ) : null}

        {serviceCount ? (
          <div className="ai-client-list">
            {!independentMode ? <ManagedServiceCard managed={managed} active={!chatRouteValue} clientStatuses={clientStatuses} serviceNames={serviceNames} pending={pendingClient} onImport={(targets, replace) => importToClients(targets, 'managed', replace)} onRemove={removeFromClient} onOpenAccount={onOpenAccount} onRefresh={onRefresh} /> : null}
            {customServices.map((service) => (
              <AiServiceRow key={service.id} service={service} active={chatRouteValue === service.id} clientStatuses={clientStatuses} serviceNames={serviceNames} pending={pendingClient} onImport={(targets, replace) => importToClients(targets, `custom:${service.id}`, replace)} onRemoveFromClient={removeFromClient} onEdit={openEditService} onRemove={(id) => void onRemoveAIService(id)} />
            ))}
          </div>
        ) : (
          <div className="ai-empty-row"><span className="ai-empty-icon"><CircleDashed size={14} /></span><span>{independentMode ? '暂无模型服务，点「新增服务」添加本机服务。' : '暂无模型服务，点「新增服务」添加本机服务，或先对接工作台。'}</span></div>
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
  opencode: 'OpenCode',
  continue: 'Continue',
  aider: 'Aider',
  crush: 'Crush',
  qoder: 'Qoder',
  'qoder-cn': 'Qoder CN',
  zcode: 'ZCode',
};

type ClientStatus = {
  target: string;
  state: string;
  client_detected: boolean;
  detail: string;
  config_path?: string;
  synced_at?: string;
  service?: string;
};

function formatSyncTime(value: string) {
  if (!value) return '';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  const pad = (input: number) => String(input).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** 工具展示名：内置映射兜底到 id，避免出现空白按钮或空标签。 */
export function clientLabel(target: string) {
  return clientLabels[target] ?? target;
}

/**
 * 行内「谁在用」。列表行只铺得下 4 个：多出来的折成 +N，
 * 完整名单挂在 title 上，这样用户不展开也能知道这个服务被谁占着。
 */
function ClientUsageChips({ clients }: { clients: ClientStatus[] }) {
  if (!clients.length) return null;
  const names = clients.map((client) => clientLabel(client.target));
  const shown = names.slice(0, 4);
  return (
    <span className="ai-service-imported-clients" title={`已分发到：${names.join('、')}`}>
      {shown.map((name) => <span className="ai-service-client-chip" key={name}>{name}</span>)}
      {names.length > shown.length ? <span className="ai-service-client-chip more">+{names.length - shown.length}</span> : null}
    </span>
  );
}

/**
 * 模型服务的行内管理面：把这个服务分发给本机 AI 工具。
 * 「HiMind AI 对话用哪个服务」是页面级单选，放在列表上方，这里只说分发。
 * 每个工具一行、一个动作：原先要先勾选再点「分发到所选」，对「把这个服务给某个
 * 工具用」这种单次操作是多余的两步，状态和动作也被拆在了两段列表里。
 * 写本机配置要一秒左右，按下的那个按钮会就地换成「分发中…」并转圈，
 * 同页其它工具不受影响，用户不用猜是不是点空了。
 */
function AiServiceDetail({ id, source, subject, meta, clientStatuses, serviceNames, pending, onImport, onRemoveFromClient }: {
  id: string;
  source: string;
  subject: string;
  meta: Array<{ label: string; value: string }>;
  clientStatuses: ClientStatus[];
  serviceNames: Record<string, string>;
  pending: ClientPending | null;
  onImport: (targets: string[], replace?: boolean) => Promise<void>;
  onRemoveFromClient: (target: string) => Promise<void>;
}) {
  const confirm = useConfirm();
  const label = clientLabel;
  // 没有服务的 imported 客户端来自旧版本或外部写入，簿记里查不到来源；
  // 这里如实标注，避免和「其他服务」混为一谈。
  const sourceLabel = (client: ClientStatus) => (client.service ? serviceNames[client.service] ?? '其他服务' : '来源不明的服务');
  const detected = clientStatuses.filter((client) => client.client_detected);
  // 未安装的工具没有任何可执行动作，收进折叠区，不占主列表的位置。
  const undetected = clientStatuses.filter((client) => !client.client_detected);
  return (
    <div className="ai-service-detail" id={id}>
      <div className="ai-service-detail-block">
        {/* 展开按钮已经写着「分发给 AI 工具」，这里只给清单命名，
            同一个说法连出现两次会让人以为要点两下。 */}
        <div className="ai-service-block-title">本机 AI 工具</div>
        {detected.length ? (
          <ul className="ai-service-binding-list">
            {detected.map((client) => {
              const usingThis = client.state === 'imported' && client.service === source;
              const usingOther = client.state === 'imported' && !usingThis;
              // 只有正在被写的那个工具变忙：其它行的按钮不该陪着一起灰掉。
              const rowAction = pending?.target === client.target ? pending.action : null;
              const rowBusy = rowAction !== null;
              return (
                <li key={client.target} className={`ai-service-binding${usingThis ? ' current' : ''}`}>
                  <span className="ai-service-binding-name">{label(client.target)}</span>
                  <span className="ai-service-binding-meta" title={client.config_path || undefined}>
                    {usingThis
                      ? ['正在使用本服务', client.synced_at ? `最近同步 ${formatSyncTime(client.synced_at)}` : ''].filter(Boolean).join(' · ')
                      : usingOther ? `当前使用 ${sourceLabel(client)}` : '未分发'}
                  </span>
                  <span className="ai-service-binding-actions">
                    {usingThis ? (
                      <button
                        type="button"
                        className="btn ai-service-tool-btn"
                        disabled={rowBusy}
                        aria-busy={rowBusy}
                        onClick={() => { void confirm({ title: `取消分发到 ${label(client.target)}？`, description: `取消后该工具恢复自己的模型配置，「${subject}」仍可再分发。`, confirmText: '取消分发' }).then((accepted) => { if (accepted) void onRemoveFromClient(client.target); }); }}
                      >
                        {rowAction === 'remove' ? <><BusyIndicator size={11} />取消分发中</> : '取消分发'}
                      </button>
                    ) : (
                      <button
                        type="button"
                        className="btn ai-service-tool-btn"
                        disabled={rowBusy}
                        aria-busy={rowBusy}
                        title={usingOther ? `分发后该工具会改用「${subject}」` : `把「${subject}」写入 ${label(client.target)} 的配置`}
                        onClick={() => {
                          if (!usingOther) { void onImport([client.target]); return; }
                          void confirm({ title: `把 ${label(client.target)} 改为使用「${subject}」？`, description: `该工具当前使用 ${sourceLabel(client)}，分发后会改用当前服务。`, confirmText: '分发' }).then((accepted) => { if (accepted) void onImport([client.target], true); });
                        }}
                      >
                        {rowAction === 'import' ? <><BusyIndicator size={11} />分发中</> : usingOther ? '改用本服务' : '分发'}
                      </button>
                    )}
                    {/* 已登记来源的工具在它自己的服务行里取消分发；来源不明的注册没有那一行，
                        只能在这里取消，否则这个状态在应用内无法消解。 */}
                    {usingOther && !client.service ? (
                      <button
                        type="button"
                        className="btn ai-service-tool-btn"
                        disabled={rowBusy}
                        aria-busy={rowBusy}
                        title="该注册无导入前快照，取消分发只会移除 HiMind 写入的供应商配置"
                        onClick={() => { void confirm({ title: `取消 ${label(client.target)} 的 HiMind 注册？`, description: '只移除 HiMind 写入的配置，工具自身配置不动；已登记来源的工具请到对应服务里取消。', confirmText: '取消分发' }).then((accepted) => { if (accepted) void onRemoveFromClient(client.target); }); }}
                      >
                        {rowAction === 'remove' ? <><BusyIndicator size={11} />取消分发中</> : '取消分发'}
                      </button>
                    ) : null}
                  </span>
                </li>
              );
            })}
          </ul>
        ) : <p className="ai-service-hint">本机还没有检测到可用的 AI 工具，装好工具后再回来分发。</p>}
        {undetected.length ? (
          <details className="ai-service-supported">
            <summary><span>未检测到本机安装的工具</span><span className="ai-service-supported-count">{undetected.length}</span></summary>
            <ul className="ai-service-supported-list">
              {undetected.map((client) => (
                <li className="undetected" key={client.target}><span>{label(client.target)}</span><span>未检测到本机安装</span></li>
              ))}
            </ul>
          </details>
        ) : null}
      </div>

      <div className="ai-service-detail-block">
        <dl className="ai-service-meta">
          {meta.map((item) => <div key={item.label}><dt>{item.label}</dt><dd title={item.value}>{item.value}</dd></div>)}
        </dl>
      </div>
    </div>
  );
}

function AiServiceRow({ service, active, clientStatuses, serviceNames, pending, onImport, onRemoveFromClient, onEdit, onRemove }: {
  service: CustomAIService;
  active: boolean;
  clientStatuses: ClientStatus[];
  serviceNames: Record<string, string>;
  pending: ClientPending | null;
  onImport: (targets: string[], replace?: boolean) => Promise<void>;
  onRemoveFromClient: (target: string) => Promise<void>;
  onEdit: (service: CustomAIService) => void;
  onRemove: (id: string) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const confirm = useConfirm();
  const source = `custom:${service.id}`;
  const boundClients = clientStatuses.filter((client) => client.state === 'imported' && client.service === source);
  // 删除只受「正在使用本服务」的工具约束：簿记里绑定到本服务时，删掉会留下悬空归属。
  // 来源不明的注册没有归属（见 AiServiceDetail），既不指向本服务、也不指向任何服务 id，
  // 因此不在这里拦删除；它们在展开区单独列出，可就地取消分发。
  const canDelete = boundClients.length === 0;
  const deleteBlockedReason = canDelete ? undefined : '有 AI 工具正在使用该服务，请先取消分发';
  // 行级菜单改的是「服务本身」（编辑/删除），和某个工具正在写配置是两件事，
  // 但同一时刻编辑/删除会和进行中的写入抢同一份服务定义，所以这里按全局忙闲挡住。
  const busy = pending !== null;
  const detailId = `ai-service-detail-${service.id}`;
  const summary = `${service.base_url} · ${protocolLabels[service.protocol] ?? service.protocol} · 模型 ${service.model}${service.models.length > 1 ? ` 等 ${service.models.length} 个` : ''}`;
  return (
    <article className={`ai-client-row ai-service-row${active ? ' active' : ''}${expanded ? ' expanded' : ''}`}>
      <div className="ai-client-icon target"><PlugZap size={18} /></div>
      <div className="ai-client-copy">
        <strong>{service.display_name}</strong>
        <span title={summary}>{summary}</span>
        <ClientUsageChips clients={boundClients} />
      </div>
      <Pill kind={boundClients.length ? 'success' : 'neutral'}>{boundClients.length ? `已分发 ${boundClients.length} 个工具` : '未分发'}</Pill>
      <div className="ai-client-registration-actions ai-service-row-actions">
        {/* 主行只留「分发给 AI 工具」这一个文字按钮：分发是这个页面的主操作，
            按钮名字直接写清点开能做什么，再靠箭头表示它是展开而不是跳转。
            编辑/删除是低频动作，收进行尾图标菜单。 */}
        <button
          type="button"
          className="btn ai-service-row-toggle"
          aria-expanded={expanded}
          aria-controls={detailId}
          onClick={() => setExpanded((value) => !value)}
        >
          分发给 AI 工具
          <ChevronDown className="ai-service-row-chevron" size={14} />
        </button>
        <ActionMenu className="ai-service-row-menu" icon={<MoreHorizontal size={15} aria-hidden="true" />} title={`${service.display_name} 的更多操作`} variant="icon">
          {close => <>
            <ActionMenuItem icon={<Pencil size={16} aria-hidden="true" />} label="编辑服务" disabled={busy} onClick={() => { close(); onEdit(service); }} />
            <ActionMenuItem
              icon={<Trash2 size={16} aria-hidden="true" />}
              label="删除服务"
              title={deleteBlockedReason}
              state={deleteBlockedReason}
              danger
              disabled={!canDelete || busy}
              onClick={() => { close(); void confirm({ title: `删除模型服务「${service.display_name}」？`, description: '删除后需要重新填写接口地址和密钥才能再使用。', confirmText: '删除' }).then((accepted) => { if (accepted) onRemove(service.id); }); }}
            />
          </>}
        </ActionMenu>
      </div>
      {expanded ? (
        <AiServiceDetail
          id={detailId}
          source={source}
          subject={service.display_name}
          meta={[
            { label: '协议', value: protocolLabels[service.protocol] ?? service.protocol },
            { label: '默认模型', value: service.model },
            { label: '模型', value: service.models.length ? `${service.models.length} 个` : '1 个' },
            { label: 'Base URL', value: service.base_url },
          ]}
          clientStatuses={clientStatuses}
          serviceNames={serviceNames}
          pending={pending}
          onImport={onImport}
          onRemoveFromClient={onRemoveFromClient}
        />
      ) : null}
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

function ManagedServiceCard({ managed, active, clientStatuses, serviceNames, pending, onImport, onRemove, onOpenAccount, onRefresh }: { managed: ManagedAIServiceSummary; active: boolean; clientStatuses: ClientStatus[]; serviceNames: Record<string, string>; pending: ClientPending | null; onImport: (targets: string[], replace?: boolean) => Promise<void>; onRemove: (target: string) => Promise<void>; onOpenAccount: () => void; onRefresh: () => void }) {
  const [expanded, setExpanded] = useState(false);
  if (managed.available) {
    const models = managed.models?.length ? `${managed.models.length} 个模型` : '未返回模型列表';
    const source = 'managed';
    const boundClients = clientStatuses.filter((client) => client.state === 'imported' && client.service === source);
    const detailId = 'ai-service-detail-managed';
    return (
      <article className={`ai-client-row ai-service-row${active ? ' active' : ''}${expanded ? ' expanded' : ''}`}>
        <div className="ai-client-icon target"><ShieldCheck size={18} /></div>
        <div className="ai-client-copy">
          <strong>工作台模型服务</strong>
          <span>{managed.model} · {models} · {managed.base_url}</span>
          <ClientUsageChips clients={boundClients} />
        </div>
        <Pill kind={boundClients.length ? 'success' : 'neutral'}>{boundClients.length ? `已分发 ${boundClients.length} 个工具` : '未分发'}</Pill>
        <div className="ai-client-registration-actions ai-service-row-actions">
          <button
            type="button"
            className="btn ai-service-row-toggle"
            aria-expanded={expanded}
            aria-controls={detailId}
            onClick={() => setExpanded((value) => !value)}
          >
            分发给 AI 工具
            <ChevronDown className="ai-service-row-chevron" size={14} />
          </button>
        </div>
        {expanded ? (
          <AiServiceDetail
            id={detailId}
            source={source}
            subject="工作台模型服务"
            meta={[
              { label: '默认模型', value: managed.model ?? '' },
              { label: '模型', value: models },
              { label: 'Base URL', value: managed.base_url ?? '' },
            ]}
            clientStatuses={clientStatuses}
            serviceNames={serviceNames}
            pending={pending}
            onImport={onImport}
            onRemoveFromClient={onRemove}
          />
        ) : null}
      </article>
    );
  }
  const reason = managed.reason ?? 'unknown';
  const reasonText: Record<string, string> = {
    not_authorized: '尚未连接工作台账号，连接后可使用工作台提供的模型服务',
    user_mismatch: '桌面端与当前工作台账号不一致，请重新连接',
    independent: '未对接 AI 工作台时不使用工作台提供的模型服务',
    no_credential: '工作台尚未生成 AI 凭据，请先选择服务渠道',
    not_ready: '当前 AI 凭据不可用，请先在工作台选择有效渠道',
    network_error: '无法连接工作台，稍后会自动重试',
    dashboard_error: '无法读取工作台模型服务，请稍后重试',
    parse_error: '暂时无法读取工作台模型服务，请刷新后重试',
  };
  return (
    <section className="ai-managed-strip">
      <div className="ai-managed-icon muted">{reason === 'not_authorized' ? <CircleDashed size={18} /> : <CircleX size={18} />}</div>
      <div className="ai-managed-copy">
        <strong>工作台模型服务未就绪</strong>
        <span>{reasonText[reason] ?? '工作台暂未提供可用的模型服务'}</span>
      </div>
      <div className="ai-managed-actions">
        {reason === 'not_authorized' || reason === 'user_mismatch' ? <button className="btn" onClick={onOpenAccount}><ExternalLink size={13} />连接账号</button> : null}
        {reason === 'no_credential' || reason === 'not_ready' ? <button className="btn" onClick={onOpenAccount}><ExternalLink size={13} />配置服务</button> : null}
        {reason === 'network_error' || reason === 'dashboard_error' || reason === 'parse_error' ? <button className="btn btn-icon" title="重新读取工作台服务" aria-label="重新读取工作台服务" onClick={onRefresh}><RefreshCw size={14} /></button> : null}
      </div>
    </section>
  );
}
