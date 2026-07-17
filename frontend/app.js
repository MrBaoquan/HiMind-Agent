const invoke =
  window.__TAURI__?.core?.invoke ||
  window.__TAURI_INTERNALS__?.invoke;

const state = {
  status: null,
  approvals: [],
  settings: null,
  logs: [],
  pluginRegistry: null,
  capabilities: [],
  loginState: null,
  currentPage: 'dashboard',
  drafts: { loginUsername: '', loginPassword: '' },
  uiMessage: null,
  loginModalOpen: false,
};

function switchPage(page, el) {
  state.currentPage = page;
  document.querySelectorAll('.sidebar nav a').forEach(a => a.classList.remove('active'));
  if (el) el.classList.add('active');
  render();
}

function render() {
  const el = document.getElementById('main-content');
  let content = '';
  switch (state.currentPage) {
    case 'dashboard': content = renderDashboard(); break;
    case 'approvals': content = renderApprovals(); break;
    case 'plugins': content = renderPlugins(); break;
    case 'settings': content = renderSettings(); break;
    case 'logs': content = renderLogs(); break;
  }
  el.innerHTML = content + renderLoginModal();
}

function loginConfigured() {
  return state.loginState?.status === 'credentials_configured';
}

function setMessage(kind, text) {
  state.uiMessage = { kind, text };
  render();
}

function clearMessage() {
  state.uiMessage = null;
}

function openLoginModal() {
  if (!state.drafts.loginUsername && state.loginState?.account) {
    state.drafts.loginUsername = state.loginState.account;
  }
  state.drafts.loginPassword = '';
  state.loginModalOpen = true;
  render();
}

function closeLoginModal() {
  state.loginModalOpen = false;
  state.drafts.loginPassword = '';
  render();
}

function formatError(error, fallback) {
  if (typeof error === 'string' && error.trim()) return error.trim();
  if (error?.message) return error.message;
  return fallback;
}

function renderMessage() {
  if (!state.uiMessage?.text) return '';
  return `<div class="banner ${state.uiMessage.kind || 'info'}">${escapeHtml(state.uiMessage.text)}</div>`;
}

function formatTimeout(s) { return s || 30; }
function escapeHtml(v) { return String(v || '').replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;'); }
function switchToSettings() { const link = document.querySelector('[data-page="settings"]'); switchPage('settings', link); }

function renderFatal(message) {
  const el = document.getElementById('main-content');
  if (!el) return;
  el.innerHTML = `<div class="card"><div class="card-header">Agent 面板初始化失败</div><div class="card-body"><div class="muted">${escapeHtml(message)}</div></div></div>`;
}

function localBase() {
  return `http://127.0.0.1:${state.status?.local_port || 18181}`;
}

async function fetchLocalJson(path, options) {
  const response = await fetch(`${localBase()}${path}`, options);
  if (!response.ok) throw new Error(await response.text());
  return await response.json();
}

function renderTags(items) {
  if (!items.length) return '<span class="muted">--</span>';
  return `<div class="tag-list">${items.map(item => `<span class="tag">${escapeHtml(item)}</span>`).join('')}</div>`;
}

async function refreshStatus() {
  try {
    state.status = await invoke('get_agent_status');
    document.getElementById('version').textContent = `v${state.status.version}`;
  } catch (e) {
    console.error('status error', e);
  }
  render();
}

async function refreshApprovals() {
  try { state.approvals = await invoke('get_pending_approvals'); } catch (e) { console.error('approvals error', e); }
  updateBadge();
  render();
}

async function refreshSettings() {
  try { state.settings = await invoke('get_approval_settings'); } catch (e) { console.error('settings error', e); }
  render();
}

async function refreshLoginStatus() {
  try { state.loginState = await invoke('get_local_login_status'); } catch (e) { console.error('login status error', e); }
  render();
}

async function refreshLogs() {
  try { state.logs = await invoke('get_agent_logs'); } catch (e) { console.error('logs error', e); }
  render();
}

async function respondApproval(id, approved) {
  try {
    clearMessage();
    await invoke('respond_approval', { id, approved });
    await refreshApprovals();
    await refreshStatus();
  } catch (e) {
    console.error('respond error', e);
    setMessage('error', formatError(e, '审批处理失败'));
  }
}

async function updateRule(requestType, mode) {
  try {
    clearMessage();
    await invoke('set_approval_rule', { requestType, mode });
    await refreshSettings();
    setMessage('success', '审批规则已更新');
  } catch (e) {
    console.error(e);
    setMessage('error', formatError(e, '审批规则更新失败'));
  }
}

async function updateTimeout(seconds) {
  try {
    clearMessage();
    await invoke('set_approval_timeout', { seconds: parseInt(seconds, 10) });
    await refreshSettings();
    setMessage('success', '审批超时已更新');
  } catch (e) {
    console.error(e);
    setMessage('error', formatError(e, '审批超时更新失败'));
  }
}

async function updateAutoStart(enabled) {
  try {
    setMessage('info', enabled ? '正在启用开机自启...' : '正在关闭开机自启...');
    const result = await invoke('set_auto_start', { enabled });
    await refreshSettings();
    await refreshLogs();
    setMessage('success', result?.auto_start ? '已启用开机自启' : '已关闭开机自启');
  } catch (e) {
    console.error(e);
    setMessage('error', formatError(e, '开机自启更新失败'));
    await refreshSettings();
  }
}

async function saveLocalLogin() {
  try {
    clearMessage();
    state.loginState = await invoke('save_local_login', { username: state.drafts.loginUsername, password: state.drafts.loginPassword });
    state.drafts.loginPassword = '';
    state.loginModalOpen = false;
    await refreshStatus();
    await refreshLoginStatus();
    await refreshLogs();
    setMessage('success', '内网账号已保存到当前 Agent');
  } catch (e) {
    console.error('save login error', e);
    setMessage('error', formatError(e, '保存内网账号失败'));
  }
}

async function logoutLocalLogin() {
  try {
    clearMessage();
    state.loginState = await invoke('logout_local_login');
    state.drafts.loginPassword = '';
    state.loginModalOpen = false;
    await refreshStatus();
    await refreshLoginStatus();
    await refreshLogs();
    setMessage('success', '已清除当前 Agent 保存的内网凭据');
  } catch (e) {
    console.error('logout login error', e);
    setMessage('error', formatError(e, '清除内网凭据失败'));
  }
}

async function openDashboard() { try { await invoke('open_dashboard_page'); } catch (e) { console.error(e); } }
async function openInnerAdminPage() { try { await invoke('open_inner_admin_page'); } catch (e) { console.error(e); } }
async function openAgentDirectory() { try { await invoke('open_agent_directory'); } catch (e) { console.error(e); } }

function updateBadge() {
  const badge = document.getElementById('approval-badge');
  if (!badge) return;
  if (state.approvals.length > 0) {
    badge.textContent = state.approvals.length;
    badge.style.display = 'inline';
  } else {
    badge.style.display = 'none';
  }
}

document.addEventListener('click', event => {
  const modalContainer = event.target.closest('[data-modal="login-editor"]');
  const target = modalContainer
    ? event.target.closest('[data-page], [data-action]') && modalContainer.contains(event.target.closest('[data-page], [data-action]'))
      ? event.target.closest('[data-page], [data-action]')
      : null
    : event.target.closest('[data-page], [data-action]');
  if (!target) return;

  const page = target.getAttribute('data-page');
  if (page) {
    event.preventDefault();
    switchPage(page, target);
    return;
  }

  if (target.tagName === 'A') {
    event.preventDefault();
  }

  const action = target.getAttribute('data-action');
  switch (action) {
    case 'open-login-modal':
      openLoginModal();
      break;
    case 'open-settings':
      switchToSettings();
      break;
    case 'close-login-modal':
      closeLoginModal();
      break;
    case 'open-dashboard':
      openDashboard();
      break;
    case 'open-inner-admin':
      openInnerAdminPage();
      break;
    case 'open-agent-directory':
      openAgentDirectory();
      break;
    case 'logout-login':
      logoutLocalLogin();
      break;
    case 'save-login':
      saveLocalLogin();
      break;
    case 'refresh-approvals':
      refreshApprovals();
      break;
    case 'refresh-logs':
      refreshLogs();
      break;
    case 'refresh-plugins':
      refreshPlugins();
      break;
    case 'open-plugin-directory':
      openPluginDirectory();
      break;
    case 'plugin-lifecycle':
      requestPluginLifecycle(target.getAttribute('data-plugin-id') || '', target.getAttribute('data-plugin-action') || '');
      break;
    case 'respond-approval':
      respondApproval(
        target.getAttribute('data-id') || '',
        target.getAttribute('data-approved') === 'true'
      );
      break;
  }
});

document.addEventListener('input', event => {
  const target = event.target;
  if (!(target instanceof HTMLInputElement)) return;
  switch (target.getAttribute('data-field')) {
    case 'login-username':
      state.drafts.loginUsername = target.value;
      break;
    case 'login-password':
      state.drafts.loginPassword = target.value;
      break;
  }
});

document.addEventListener('change', event => {
  const target = event.target;
  if (!(target instanceof HTMLElement)) return;
  const action = target.getAttribute('data-action');
  if (!action) return;

  if (action === 'update-rule' && target instanceof HTMLSelectElement) {
    updateRule(target.getAttribute('data-request-type') || '', target.value);
    return;
  }

  if (action === 'update-timeout' && target instanceof HTMLSelectElement) {
    updateTimeout(target.value);
    return;
  }

  if (action === 'update-auto-start' && target instanceof HTMLInputElement) {
    updateAutoStart(target.checked);
  }
});

render();

(async function init() {
  if (!invoke) {
    renderFatal('当前 Tauri 运行时未注入 invoke 接口，面板无法调用本地命令。');
    return;
  }
  try {
    await refreshStatus();
    await refreshApprovals();
    await refreshSettings();
    await refreshLoginStatus();
    await refreshPlugins();
    await refreshLogs();
    setInterval(async () => {
      await refreshStatus();
      await refreshApprovals();
      await refreshLoginStatus();
    }, 5000);
  } catch (e) {
    console.error('init error', e);
    renderFatal(e?.message || String(e));
  }
})();
