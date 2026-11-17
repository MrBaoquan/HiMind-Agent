/** Framework-neutral plugin client. Inventory, credentials and execution stay in Agent. */
export const SDK_VERSION = 1;

export function normalizeCapability(capability) {
  return capability === 'media.ai.describe' ? 'vision.describe' : capability.replace(/^media\./, '');
}

export function validateSelection(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('请选择服务与模型');
  const keys = value.mode === 'agent_default' ? ['mode']
    : value.mode === 'service_default' ? ['mode', 'service_id']
      : value.mode === 'pinned' ? ['mode', 'service_id', 'model'] : [];
  if (!keys.length || Object.keys(value).some(key => !keys.includes(key))
    || keys.some(key => typeof value[key] !== 'string' || !value[key].trim())) throw new Error('服务与模型选择无效');
  return Object.fromEntries(keys.map(key => [key, value[key].trim()]));
}

/** No privileged host commands: the view identity is verified by Agent on every call. */
export function viewTransport(invoke = globalThis.__TAURI_INTERNALS__?.invoke) {
  if (!invoke) throw new Error('请在 HiMind Agent 插件窗口中打开');
  return (capability, input) => invoke('invoke_plugin_view_capability', { capabilityId: capability, input });
}

/** For Node backends with declared capability_dependencies. Never expose broker tokens to a view. */
export function brokerTransport({ url, token, fetch: request = globalThis.fetch }) {
  const endpoint = new URL(url);
  if (endpoint.protocol !== 'http:' || !['127.0.0.1', 'localhost', '[::1]'].includes(endpoint.hostname)) throw new Error('能力代理必须是本机地址');
  return async (capability, input) => {
    const response = await request(new URL('/invoke', endpoint), {
      method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${token}` },
      body: JSON.stringify({ capability, input }),
    });
    const body = await response.json();
    if (!response.ok || body.error) throw new Error(body.error?.message || body.error || `能力调用失败 ${response.status}`);
    return body.result ?? body;
  };
}

function unwrap(result) {
  if (result?.isError) throw new Error(result.content?.find(item => item.type === 'text')?.text || '能力调用失败');
  if (result?.structuredContent) return result.structuredContent;
  if (result?.content) {
    const item = result.content.find(item => item.type === 'text');
    if (item) { try { return JSON.parse(item.text); } catch { /* normal result below */ } }
  }
  return result;
}

export function createAiClient({ invoke, pluginId, projectId, storage = globalThis.localStorage }) {
  if (!pluginId || typeof invoke !== 'function') throw new Error('需要插件 ID 与能力调用接口');
  const call = async (capability, input) => unwrap(await invoke(capability, input));
  const key = (capability, scope) => JSON.stringify(['himind.ai.selection', SDK_VERSION, pluginId,
    scope === 'project' ? projectId : null, normalizeCapability(capability)]);
  function read(capability, scope) {
    if (scope === 'project' && !projectId) return null;
    const raw = storage?.getItem(key(capability, scope));
    if (!raw) return null;
    try {
      const value = JSON.parse(raw);
      if (value.fallback && !['none', 'configured'].includes(value.fallback)) return null;
      return { selection: validateSelection(value.selection), ...(value.fallback ? { fallback: value.fallback } : {}) };
    } catch { return null; }
  }
  function effective(capability, override) {
    let source = 'invocation';
    let value = override;
    if (!value) { value = read(capability, 'project'); source = 'project'; }
    if (!value) { value = read(capability, 'plugin'); source = 'plugin'; }
    if (!value) { value = { selection: { mode: 'agent_default' } }; source = 'agent_default'; }
    if (value.fallback && !['none', 'configured'].includes(value.fallback)) throw new Error('回退策略无效');
    return { selection: validateSelection(value.selection), selection_source: source,
      ...(value.fallback ? { fallback: value.fallback } : {}) };
  }
  return {
    effective,
    catalog: (filters = {}) => call('media.provider.catalog', filters),
    save(capability, value, scope = 'project') {
      if (!['plugin', 'project'].includes(scope) || (scope === 'project' && !projectId)) throw new Error('当前项目不可保存偏好');
      const checked = effective(capability, value);
      if (!storage) throw new Error('当前客户端不支持保存偏好');
      storage.setItem(key(capability, scope), JSON.stringify({ selection: checked.selection, fallback: checked.fallback }));
    },
    clear(capability, scope = 'project') {
      if (!['plugin', 'project'].includes(scope) || (scope === 'project' && !projectId)) throw new Error('当前项目不可清除偏好');
      storage?.removeItem(key(capability, scope));
    },
    plan: (capability, { payload, ...override } = {}) => call('media.route.plan', {
      capability, ...effective(capability, override.selection ? override : undefined), ...(payload ? { payload } : {}),
    }),
    execute(capability, payload, override) {
      if (['service', 'model', 'selection', 'selection_source', 'fallback'].some(key => key in payload)) throw new Error('请通过选择参数指定服务与模型');
      return call(capability, { ...payload, ...effective(capability, override) });
    },
    /** Resolve once for a batch; subsequent calls keep this exact service/model. */
    async pin(capability, override) {
      const plan = await call('media.route.plan', { capability, ...effective(capability, override) });
      if (!plan.selected?.eligible || !plan.selected.model) throw new Error('所选服务或模型当前不可执行');
      return { selection: { mode: 'pinned', service_id: plan.selected.service_id, model: plan.selected.model }, fallback: 'none' };
    },
    async voices(target, filters = {}) {
      const plan = await call('media.route.plan', { capability: 'audio.speech', ...effective('audio.speech', target) });
      if (!plan.selected?.eligible) throw new Error('所选语音服务不可执行');
      const catalog = await call('media.provider.catalog', { capability: 'audio.speech' });
      const binding = catalog.providers.find(item => item.id === plan.selected.service_id)?.media_capabilities.find(item => item.capability === 'audio.speech');
      if (binding?.voice_catalog !== 'supported') return { state: 'unsupported', voices: [], service_id: plan.selected.service_id };
      return call('media.audio.voice.list', { ...filters, service: `custom:${plan.selected.service_id}` });
    },
  };
}
