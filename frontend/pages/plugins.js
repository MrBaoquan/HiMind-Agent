function renderPlugins() {
  const pluginItems = state.pluginRegistry?.items || [];
  const pluginCapabilities = state.capabilities.filter(item => String(item.source || '').startsWith('plugin:'));
  return `
    <h2>插件管理</h2>
    ${renderMessage()}
    <div class="status-grid">
      <div class="status-card"><div class="label">注册表</div><div class="value ${state.pluginRegistry?.registry_ready ? 'online' : 'offline'}">${state.pluginRegistry?.registry_ready ? '就绪' : '未就绪'}</div><div class="subvalue">${escapeHtml(state.pluginRegistry?.registry_dir || '--')}</div></div>
      <div class="status-card"><div class="label">已安装</div><div class="value">${state.pluginRegistry?.total ?? pluginItems.length}</div></div>
      <div class="status-card"><div class="label">能力数量</div><div class="value">${pluginCapabilities.length}</div></div>
      <div class="status-card"><div class="label">运行时</div><div class="value" style="font-size:16px">${escapeHtml(state.pluginRegistry?.external_runtime || 'JSON-RPC')}</div></div>
    </div>
    <div class="card">
      <div class="card-header">插件注册表 <div class="actions-row"><button class="btn" data-action="refresh-plugins">刷新</button><button class="btn" data-action="open-plugin-directory" ${state.pluginRegistry?.registry_dir ? '' : 'disabled'}>打开插件目录</button></div></div>
      <div class="card-body">
        ${pluginItems.length === 0 ? '<div class="empty"><div class="icon">&#128295;</div>暂无插件</div>' : renderPluginTable(pluginItems)}
      </div>
    </div>
    <div class="card">
      <div class="card-header">能力清单</div>
      <div class="card-body">
        ${state.capabilities.length === 0 ? '<div class="empty"><div class="icon">&#8987;</div>暂无数据</div>' : renderCapabilityTable(state.capabilities)}
      </div>
    </div>
  `;
}

function renderPluginTable(items) {
  return `<div class="table-wrap"><table><thead><tr><th>插件</th><th>版本</th><th>状态</th><th>能力</th><th>权限</th><th>操作</th></tr></thead><tbody>${items.map(item => `
    <tr>
      <td><div><strong>${escapeHtml(item.name || item.id)}</strong></div><div class="muted code-inline">${escapeHtml(item.id)}</div>${item.error ? `<div class="field-hint" style="color:var(--danger)">${escapeHtml(item.error)}</div>` : ''}</td>
      <td>${escapeHtml(item.version || '--')}<div class="muted code-inline">${escapeHtml(item.runtime || '--')}</div></td>
      <td><span class="pill ${item.status === 'installed' && item.enabled ? 'success' : item.status === 'failed' ? 'danger' : 'warn'}">${escapeHtml(item.enabled ? item.status : 'disabled')}</span></td>
      <td>${renderTags((item.capabilities || []).map(capability => capability.id))}</td>
      <td>${renderTags(item.permissions || [])}</td>
      <td><div class="actions-row"><button class="btn" data-action="plugin-lifecycle" data-plugin-id="${escapeHtml(item.id)}" data-plugin-action="enable">启用</button><button class="btn" data-action="plugin-lifecycle" data-plugin-id="${escapeHtml(item.id)}" data-plugin-action="disable">停用</button><button class="btn" data-action="plugin-lifecycle" data-plugin-id="${escapeHtml(item.id)}" data-plugin-action="update">升级</button><button class="btn btn-danger" data-action="plugin-lifecycle" data-plugin-id="${escapeHtml(item.id)}" data-plugin-action="uninstall">卸载</button></div></td>
    </tr>`).join('')}</tbody></table></div>`;
}

function renderCapabilityTable(items) {
  return `<div class="table-wrap"><table><thead><tr><th>能力 ID</th><th>名称</th><th>来源</th><th>风险</th><th>说明</th></tr></thead><tbody>${items.map(item => `
    <tr>
      <td class="code-inline">${escapeHtml(item.id)}</td>
      <td>${escapeHtml(item.name || '--')}</td>
      <td><span class="tag">${escapeHtml(item.source || '--')}</span></td>
      <td>${escapeHtml(item.risk_level || '--')}</td>
      <td>${escapeHtml(item.description || '--')}</td>
    </tr>`).join('')}</tbody></table></div>`;
}

async function refreshPlugins() {
  try {
    clearMessage();
    const [registry, capabilities] = await Promise.all([
      invoke('get_plugin_registry'),
      invoke('get_agent_capabilities'),
    ]);
    state.pluginRegistry = registry;
    state.capabilities = Array.isArray(capabilities) ? capabilities : [];
  } catch (e) {
    console.error('plugins error', e);
    setMessage('error', formatError(e, '插件注册表读取失败，请确认当前运行的是最新 Agent'));
    return;
  }
  render();
}

async function openPluginDirectory() {
  try {
    await invoke('open_plugin_directory');
  } catch (e) {
    console.error(e);
    setMessage('error', formatError(e, '打开插件目录失败'));
  }
}

async function requestPluginLifecycle(pluginId, action) {
  try {
    await fetchLocalJson(`/plugins/${action}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ plugin_id: pluginId }),
    });
    await refreshPlugins();
  } catch (e) {
    console.warn(e);
    setMessage('info', '插件生命周期入口已预留；需接入分发策略、制品校验和安装状态回报后开放。');
  }
}
