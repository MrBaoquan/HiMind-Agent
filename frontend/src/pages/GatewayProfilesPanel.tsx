import { useMemo, useState } from 'react';
import {
  ChevronDown,
  ChevronUp,
  CircleAlert,
  Pencil,
  Plus,
  Route,
  Save,
  Trash2,
  X,
} from 'lucide-react';
import { useConfirm } from '../components/ConfirmDialog';
import { BusyIndicator } from '../components/BusyIndicator';
import { Pill } from '../components/Common';
import { serviceSupportsCapability } from './aiServiceCapabilities';
import {
  GATEWAY_CLIENT_HINTS,
  GATEWAY_ENTRY_PROTOCOLS,
  GATEWAY_PROTOCOL_LABELS,
  GATEWAY_PROTOCOL_SHORT_LABELS,
  compatibleEntryProtocolsForUpstream,
  gatewayCompatibilityMessage,
  supportedUpstreamLabelForEntry,
  upstreamSupportsEntry,
  gatewayProfileProtocols,
  gatewayProtocolSummary,
  serviceSupportsGatewayProtocols,
  setGatewayEntryProtocol,
  visibleProfileModels,
} from './gatewayProfileView';
import type {
  AIServiceListResult,
  AIServiceProtocol,
  CustomAIService,
  GatewayModelRoute,
  GatewayProfile,
  GatewayProfileInput,
} from '../services/agentApi';

type GatewayProfilesPanelProps = {
  profiles: GatewayProfile[];
  aiServices: AIServiceListResult | null;
  busy: boolean;
  onSave: (input: GatewayProfileInput) => Promise<void>;
  onRemove: (id: string) => Promise<void>;
};

type PanelError = {
  title: string;
  message: string;
};

const MODEL_PREVIEW_LIMIT = 3;

function routeAlias(serviceId: string, model: string) {
  return `${serviceId.trim()}/${model.trim()}`;
}

function emptyProfile(): GatewayProfileInput {
  return {
    id: '',
    name: '',
    entry_protocol: 'openai-responses',
    entry_protocols: ['openai-responses'],
    default_model: '',
    enabled: true,
    routes: [],
  };
}

function profileToInput(profile: GatewayProfile): GatewayProfileInput {
  const entryProtocols = gatewayProfileProtocols(profile);
  return {
    id: profile.id,
    name: profile.name,
    entry_protocol: entryProtocols[0] ?? profile.entry_protocol,
    entry_protocols: entryProtocols,
    default_model: profile.default_model,
    enabled: profile.enabled,
    routes: profile.routes.map(route => ({ ...route })),
  };
}

export function GatewayProfilesPanel({ profiles, aiServices, busy, onSave, onRemove }: GatewayProfilesPanelProps) {
  const confirm = useConfirm();
  const [draft, setDraft] = useState<GatewayProfileInput | null>(null);
  const [error, setError] = useState<PanelError | null>(null);
  const [pendingAction, setPendingAction] = useState<string | null>(null);
  const [expandedModels, setExpandedModels] = useState<Set<string>>(() => new Set());
  const services = useMemo(
    () => (aiServices?.custom?.services ?? []).filter(service => serviceSupportsCapability(service, 'chat')),
    [aiServices],
  );
  const anyActionPending = busy || Boolean(pendingAction);

  function modelsFor(serviceId: string) {
    const service = services.find(item => item.id === serviceId);
    if (!service) return [];
    return service.models.length ? service.models : (service.model ? [service.model] : []);
  }

  function serviceFor(id: string) {
    return services.find(service => service.id === id);
  }

  function routeCompatibilityIssue(route: GatewayModelRoute, entryProtocols: AIServiceProtocol[]): string {
    const service = serviceFor(route.service_id);
    if (!service) return '服务已不存在，请重新选择。';
    if (!serviceSupportsGatewayProtocols(service, entryProtocols)) {
      return GATEWAY_PROTOCOL_LABELS[service.protocol] + ' 上游与当前客户端不兼容。';
    }
    return '';
  }

  function startCreate() {
    setError(null);
    setDraft(emptyProfile());
  }

  function startEdit(profile: GatewayProfile) {
    setError(null);
    setDraft(profileToInput(profile));
  }

  function toggleProtocol(protocol: AIServiceProtocol, selected: boolean) {
    if (!draft) return;
    const entryProtocols = setGatewayEntryProtocol(draft.entry_protocols, protocol, selected);
    setDraft({
      ...draft,
      entry_protocol: entryProtocols[0],
      entry_protocols: entryProtocols,
    });
  }

  function addRoute() {
    if (!draft) return;
    const compatible = services.filter(service => serviceSupportsGatewayProtocols(service, draft.entry_protocols));
    const service = compatible.find(item => !draft.routes.some(route => route.service_id === item.id)) ?? compatible[0];
    if (!service) {
      setError({
        title: '没有兼容的模型服务',
        message: '当前协议下没有可用的 Chat 服务。',
      });
      return;
    }
    const model = modelsFor(service.id)[0] ?? '';
    setError(null);
    setDraft({
      ...draft,
      routes: [...draft.routes, {
        exposed_model: routeAlias(service.id, model),
        service_id: service.id,
        upstream_model: model,
        enabled: true,
      }],
    });
  }

  function updateRoute(index: number, patch: Partial<GatewayModelRoute>) {
    if (!draft) return;
    const routes = draft.routes.map((route, position) => position === index ? { ...route, ...patch } : route);
    const next = { ...draft, routes };
    if (!next.default_model) next.default_model = routes[index]?.exposed_model ?? '';
    setDraft(next);
  }

  function changeRouteService(index: number, serviceId: string) {
    const model = modelsFor(serviceId)[0] ?? '';
    updateRoute(index, {
      service_id: serviceId,
      upstream_model: model,
      exposed_model: routeAlias(serviceId, model),
    });
  }

  function removeRoute(index: number) {
    if (!draft) return;
    const routes = draft.routes.filter((_, position) => position !== index);
    const defaultStillEnabled = routes.some(route => route.enabled && route.exposed_model === draft.default_model);
    setDraft({
      ...draft,
      routes,
      default_model: defaultStillEnabled
        ? draft.default_model
        : (routes.find(route => route.enabled)?.exposed_model ?? ''),
    });
  }

  async function runAction(action: string, work: () => Promise<void>) {
    setPendingAction(action);
    setError(null);
    try {
      await work();
    } catch (reason) {
      setError({
        title: action === 'save' ? '聚合入口未保存' : '操作未完成',
        message: reason instanceof Error ? reason.message : String(reason),
      });
    } finally {
      setPendingAction(null);
    }
  }

  async function save() {
    if (!draft) return;
    if (!draft.id.trim() || !draft.name.trim()) {
      setError({ title: '缺少必要信息', message: '请填写入口 ID 和名称。' });
      return;
    }
    if (!draft.entry_protocols.length) {
      setError({ title: '缺少客户端协议', message: '至少选择一个客户端协议。' });
      return;
    }
    if (!draft.routes.length) {
      setError({ title: '缺少模型', message: '至少添加一个模型。' });
      return;
    }
    if (draft.enabled && !draft.routes.some(route => route.enabled)) {
      setError({ title: '没有启用模型', message: '请至少启用一个模型。' });
      return;
    }
    const incompatible = draft.routes
      .map(route => routeCompatibilityIssue(route, draft.entry_protocols))
      .find(Boolean);
    if (incompatible) {
      setError({ title: '模型不兼容', message: incompatible });
      return;
    }
    const missingClient = draft.entry_protocols.find(entry => !draft.routes.some(route => {
      const service = serviceFor(route.service_id);
      return route.enabled && Boolean(service && upstreamSupportsEntry(service.protocol, entry));
    }));
    if (draft.enabled && missingClient) {
      setError({
        title: '客户端缺少可用模型',
        message: GATEWAY_PROTOCOL_SHORT_LABELS[missingClient] + ' 没有可用模型。',
      });
      return;
    }
    const entryProtocols = [...draft.entry_protocols];
    await runAction('save', async () => {
      await onSave({
        ...draft,
        entry_protocol: entryProtocols[0],
        entry_protocols: entryProtocols,
        routes: draft.routes.map(route => ({ ...route, exposed_model: route.exposed_model.trim() })),
      });
      setDraft(null);
    });
  }

  async function remove(profile: GatewayProfile) {
    const accepted = await confirm({
      title: '删除聚合入口「' + profile.name + '」？',
      description: '正在被 AI 工具使用时无法删除。',
      confirmText: '删除',
    });
    if (!accepted) return;
    await runAction('remove:' + profile.id, async () => {
      await onRemove(profile.id);
    });
  }

  function toggleModelExpansion(id: string) {
    setExpandedModels(current => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function actionBusy(action: string) {
    return pendingAction === action;
  }

  return <section className="ai-client-section gateway-profile-section">
    <div className="ai-section-heading gateway-profile-heading">
      <div>
        <h3>模型网关</h3>
        <span>{profiles.length ? profiles.length + ' 个入口' : '暂无入口'}</span>
      </div>
      <button type="button" className="btn btn-primary" disabled={anyActionPending || Boolean(draft)} onClick={startCreate}><Plus size={14} />新建聚合入口</button>
    </div>

    {error ? <div className="development-project-error gateway-profile-error" role="alert"><CircleAlert size={15} /><div><strong>{error.title}</strong><span>{error.message}</span></div></div> : null}

    {draft ? <div className="gateway-profile-editor">
      <div className="gateway-profile-form-row">
        <label><span>入口 ID</span><input value={draft.id} disabled={profiles.some(profile => profile.id === draft.id)} placeholder="coding-models" onChange={event => setDraft({ ...draft, id: event.target.value })} /></label>
        <label><span>名称</span><input value={draft.name} placeholder="编码模型聚合" onChange={event => setDraft({ ...draft, name: event.target.value })} /></label>
        <label><span>默认模型</span><select value={draft.default_model} onChange={event => setDraft({ ...draft, default_model: event.target.value })}>
          <option value="">自动使用第一个</option>
          {draft.routes.filter(route => route.enabled).map(route => <option key={route.exposed_model} value={route.exposed_model}>{route.exposed_model}</option>)}
        </select></label>
        <label className="gateway-profile-enabled"><span>入口状态</span><span><input type="checkbox" checked={draft.enabled} onChange={event => setDraft({ ...draft, enabled: event.target.checked })} /><strong>{draft.enabled ? '启用' : '停用'}</strong></span></label>
      </div>

      <fieldset className="gateway-protocol-picker">
        <legend>客户端协议</legend>
        <div className="gateway-protocol-options">
          {GATEWAY_ENTRY_PROTOCOLS.map(protocol => {
            const selected = draft.entry_protocols.includes(protocol);
            const onlySelection = selected && draft.entry_protocols.length === 1;
            return <label className={'gateway-protocol-option' + (selected ? ' selected' : '')} key={protocol}>
              <input
                type="checkbox"
                checked={selected}
                disabled={onlySelection}
                onChange={event => toggleProtocol(protocol, event.target.checked)}
              />
              <span><strong>{GATEWAY_PROTOCOL_LABELS[protocol]}</strong><small>{GATEWAY_CLIENT_HINTS[protocol]}</small></span>
            </label>;
          })}
        </div>
        <div className="gateway-protocol-compatibility" role="status">
          <strong>{gatewayCompatibilityMessage(draft.entry_protocols)}</strong>
          <div className="gateway-protocol-coverage">
            {draft.entry_protocols.map(protocol => <div key={protocol}>
              <span>{GATEWAY_PROTOCOL_SHORT_LABELS[protocol]}</span>
              <small>{supportedUpstreamLabelForEntry(protocol)}</small>
            </div>)}
          </div>
        </div>
      </fieldset>

      <div className="gateway-route-table">
        <div className="gateway-route-head"><span>服务</span><span>上游模型</span><span>模型别名</span><span>状态</span><span>操作</span></div>
        {draft.routes.map((route, index) => {
          const compatibilityIssue = routeCompatibilityIssue(route, draft.entry_protocols);
          const routeService = serviceFor(route.service_id);
          const routeClients = routeService
            ? compatibleEntryProtocolsForUpstream(routeService.protocol, draft.entry_protocols)
            : [];
          return <div className="gateway-route-edit-row" key={route.service_id + ':' + index}>
            <label className="gateway-route-field">
              <span className="gateway-route-field-label">服务</span>
              <select value={route.service_id} aria-invalid={Boolean(compatibilityIssue)} onChange={event => changeRouteService(index, event.target.value)}>
                {services.map(service => {
                  const compatible = serviceSupportsGatewayProtocols(service, draft.entry_protocols);
                  return <option key={service.id} value={service.id} disabled={!compatible && service.id !== route.service_id}>
                    {service.display_name} · {GATEWAY_PROTOCOL_LABELS[service.protocol]}{compatible ? '' : '（不兼容）'}
                  </option>;
                })}
              </select>
            </label>
            <label className="gateway-route-field">
              <span className="gateway-route-field-label">上游模型</span>
              <select value={route.upstream_model} onChange={event => updateRoute(index, { upstream_model: event.target.value, exposed_model: routeAlias(route.service_id, event.target.value) })}>
                {modelsFor(route.service_id).map(model => <option key={model} value={model}>{model}</option>)}
              </select>
            </label>
            <label className="gateway-route-field">
              <span className="gateway-route-field-label">模型别名</span>
              <input value={route.exposed_model} placeholder="服务ID/模型ID" aria-invalid={Boolean(compatibilityIssue)} onChange={event => updateRoute(index, { exposed_model: event.target.value })} />
            </label>
            <label className="gateway-route-enabled"><input type="checkbox" checked={route.enabled} onChange={event => updateRoute(index, { enabled: event.target.checked })} />启用</label>
            <button type="button" className="btn btn-danger-quiet gateway-route-remove" aria-label="删除模型路由" onClick={() => removeRoute(index)}><Trash2 size={13} /><span>删除</span></button>
            {compatibilityIssue
              ? <span className="gateway-route-issue" role="note"><CircleAlert size={12} />{compatibilityIssue}</span>
              : <span className="gateway-route-clients">客户端：{routeClients.map(protocol => GATEWAY_PROTOCOL_SHORT_LABELS[protocol]).join(' / ')}</span>}
          </div>;
        })}
        {!draft.routes.length ? <div className="gateway-route-empty">暂无模型</div> : null}
      </div>

      <div className="gateway-profile-actions">
        <button type="button" className="btn" disabled={anyActionPending} onClick={addRoute}><Plus size={13} />添加模型</button>
        <button type="button" className="btn" disabled={anyActionPending} onClick={() => setDraft(null)}><X size={13} />取消</button>
        <button type="button" className="btn btn-primary" aria-busy={actionBusy('save')} disabled={anyActionPending} onClick={() => void save()}>{actionBusy('save') ? <BusyIndicator size={12} /> : <Save size={13} />}保存入口</button>
      </div>
    </div> : null}

    <div className="gateway-profile-list">
      {profiles.map(profile => {
        const entryProtocols = gatewayProfileProtocols(profile);
        const enabledRoutes = profile.routes.filter(route => route.enabled);
        const expanded = expandedModels.has(profile.id);
        const visibleModels = visibleProfileModels(enabledRoutes, expanded, MODEL_PREVIEW_LIMIT);
        const toggleAction = 'toggle:' + profile.id;
        const removeAction = 'remove:' + profile.id;
        return <article className={'gateway-profile-card' + (profile.enabled ? '' : ' disabled')} key={profile.id}>
          <div className="gateway-profile-icon"><Route size={18} /></div>
          <div className="gateway-profile-copy">
            <div><strong>{profile.name}</strong><Pill kind={profile.enabled ? 'success' : 'neutral'}>{profile.enabled ? '启用' : '停用'}</Pill></div>
            <code>{profile.id}</code>
            <div className="gateway-profile-meta">
              <span>{gatewayProtocolSummary(entryProtocols)}</span>
              <span>{enabledRoutes.length} 个模型</span>
              <span>默认 {profile.default_model || '自动'}</span>
            </div>
            <div className="gateway-profile-model-chips">
              {visibleModels.map(route => <span key={route.exposed_model}>{route.exposed_model}</span>)}
              {enabledRoutes.length > MODEL_PREVIEW_LIMIT ? <button
                type="button"
                className="gateway-profile-more"
                aria-expanded={expanded}
                onClick={() => toggleModelExpansion(profile.id)}
              >{expanded ? <ChevronUp size={12} /> : <ChevronDown size={12} />}{expanded ? '收起' : '+' + (enabledRoutes.length - MODEL_PREVIEW_LIMIT)}</button> : null}
            </div>
          </div>
          <div className="gateway-profile-card-actions">
            <button type="button" className="btn ai-service-tool-btn" aria-busy={actionBusy(toggleAction)} disabled={anyActionPending || Boolean(draft)} onClick={() => void runAction(toggleAction, async () => {
              await onSave({ ...profileToInput(profile), enabled: !profile.enabled });
            })}>{actionBusy(toggleAction) ? <BusyIndicator size={12} /> : null}{profile.enabled ? '停用' : '启用'}</button>
            <button type="button" className="btn ai-service-tool-btn" disabled={anyActionPending || Boolean(draft)} onClick={() => startEdit(profile)}><Pencil size={13} />编辑</button>
            <button type="button" className="btn btn-danger-quiet" aria-busy={actionBusy(removeAction)} disabled={anyActionPending || Boolean(draft)} onClick={() => void remove(profile)}>{actionBusy(removeAction) ? <BusyIndicator size={12} /> : <Trash2 size={13} />}删除</button>
          </div>
        </article>;
      })}
      {!profiles.length && !draft ? <div className="gateway-profile-empty"><Route size={20} /><strong>暂无聚合入口</strong><span>新建入口后添加模型。</span></div> : null}
    </div>
  </section>;
}
