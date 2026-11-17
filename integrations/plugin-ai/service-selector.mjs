import { normalizeCapability, validateSelection } from './client.mjs';

/** Native controls, no framework dependency. All inventory comes from Agent. */
export function mountServiceSelector(container, { client, capability, onChange = () => {}, filters = {} }) {
  const root = document.createElement('fieldset');
  root.className = 'himind-ai-selector';
  root.innerHTML = `<legend>AI 服务与模型</legend>
    <label>服务 <select data-service><option value="">跟随 Agent 默认</option></select></label>
    <label>模型 <select data-model><option value="">跟随服务默认模型</option></select></label>
    <label><input type="checkbox" data-fallback>失败时允许使用 Agent 已配置的备用服务</label>
    <p data-status role="status">正在读取可用服务…</p>
    <button type="button" data-refresh>刷新可用服务</button>
    <button type="button" data-project>记住本项目选择</button>
    <button type="button" data-plugin>设为插件默认</button>`;
  container.append(root);
  const service = root.querySelector('[data-service]');
  const model = root.querySelector('[data-model]');
  const fallback = root.querySelector('[data-fallback]');
  const status = root.querySelector('[data-status]');
  const refresh = root.querySelector('[data-refresh]');
  const saves = [...root.querySelectorAll('[data-project],[data-plugin]')];
  const cap = normalizeCapability(capability);
  let current = client.effective(capability);
  let providers = [];
  let revision = 0;
  let disposed = false;
  let ready = false;
  const option = (text, value, disabled = false) => {
    const item = new Option(text, value); item.disabled = disabled; return item;
  };
  const value = () => ({
    selection: !service.value ? { mode: 'agent_default' }
      : !model.value ? { mode: 'service_default', service_id: service.value }
        : { mode: 'pinned', service_id: service.value, model: model.value },
    fallback: fallback.checked ? 'configured' : 'none',
  });
  function fillModels(selected = '') {
    const binding = providers.find(item => item.id === service.value)?.media_capabilities.find(item => item.capability === cap);
    model.replaceChildren(option(service.value ? `跟随服务默认模型${binding ? ' · ' + binding.model : ''}` : '跟随 Agent 默认模型', ''));
    for (const name of binding?.models || []) model.add(option(name, name));
    if (selected && ![...model.options].some(item => item.value === selected)) model.add(option(`${selected}（已不可用，请重新选择）`, selected));
    model.value = selected;
    model.disabled = !service.value;
  }
  async function preflight() {
    const turn = ++revision;
    ready = false;
    saves.forEach(button => { button.disabled = true; });
    onChange({ value: value(), ready: false, targetChanged: true });
    status.textContent = '正在核对当前选择…';
    try {
      const chosen = value();
      const plan = await client.plan(capability, chosen);
      if (disposed || turn !== revision) return;
      const allowed = providers.some(item => item.id === plan.selected?.service_id
        && item.media_capabilities.some(binding => binding.capability === cap && binding.eligible));
      ready = plan.state === 'resolved' && plan.selected?.eligible === true && allowed;
      status.textContent = ready ? `将使用 ${plan.selected.display_name} / ${plan.selected.model} · 上游连接未在此处测试`
        : `当前选择不可执行（${plan.reason}），请更换服务、模型或在 Agent 中配置。`;
      saves.forEach(button => { button.disabled = !ready; });
      onChange({ value: chosen, ready, plan, targetChanged: false });
    } catch (error) {
      if (disposed || turn !== revision) return;
      status.textContent = `无法核对服务：${error.message}`;
      onChange({ value: value(), ready: false, error, targetChanged: false });
    }
  }
  async function load() {
    const turn = ++revision;
    ready = false;
    refresh.disabled = true;
    service.disabled = model.disabled = true;
    saves.forEach(button => { button.disabled = true; });
    onChange({ value: current, ready: false, targetChanged: true });
    try {
      const catalog = await client.catalog({ ...filters, capability });
      if (disposed || turn !== revision) return;
      providers = catalog.providers;
      service.replaceChildren(option('跟随 Agent 默认', ''));
      for (const item of providers) {
        const binding = item.media_capabilities.find(item => item.capability === cap);
        service.add(option(item.display_name + (binding?.eligible ? '' : '（当前不可执行）'), item.id, !binding?.eligible));
      }
      const chosen = validateSelection(current.selection);
      const id = chosen.mode === 'agent_default' ? '' : chosen.service_id;
      if (id && !providers.some(item => item.id === id)) service.add(option(`${id}（已不可用，请重新选择）`, id));
      service.value = id;
      fillModels(chosen.mode === 'pinned' ? chosen.model : '');
      fallback.checked = current.fallback ? current.fallback === 'configured' : chosen.mode === 'agent_default';
      service.disabled = false;
      await preflight();
    } catch (error) {
      if (disposed || turn !== revision) return;
      status.textContent = `无法读取服务：${error.message}。可刷新重试。`;
      onChange({ value: current, ready: false, error });
    } finally { if (!disposed) refresh.disabled = false; }
  }
  service.addEventListener('change', () => {
    fillModels(); fallback.checked = !service.value; current = value(); void preflight();
  });
  model.addEventListener('change', () => { current = value(); void preflight(); });
  fallback.addEventListener('change', () => { current = value(); void preflight(); });
  refresh.addEventListener('click', () => { current = value(); void load(); });
  saves.forEach(button => button.addEventListener('click', () => {
    if (!ready) return;
    try { client.save(capability, value(), button.hasAttribute('data-project') ? 'project' : 'plugin'); status.textContent += ' · 选择已保存'; }
    catch (error) { status.textContent = error.message; }
  }));
  const loaded = load();
  return { loaded, get value() { return value(); }, get ready() { return ready; }, destroy() { disposed = true; revision++; root.remove(); } };
}
