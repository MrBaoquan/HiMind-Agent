function renderDashboard() {
  if (!state.status) return '<div class="empty"><div class="icon">&#8987;</div>加载中...</div>';
  const workerOnline = state.status.dashboard_worker_online;
  const loginOnline = loginConfigured();
  return `
    <h2>总览</h2>
    ${renderMessage()}
    <div class="status-grid">
      <div class="status-card"><div class="label">运行状态</div><div class="value ${workerOnline ? 'online' : 'offline'}">${workerOnline ? '在线' : '离线'}</div></div>
      <div class="status-card"><div class="label">版本</div><div class="value">${state.status.version}</div></div>
      <div class="status-card"><div class="label">Dashboard</div><div class="value" style="font-size:16px">${state.status.dashboard_base || '--'}</div><div class="subvalue">${state.status.dashboard_agent_id || ''}</div></div>
      <div class="status-card"><div class="label">本地端口</div><div class="value" style="font-size:16px">${state.status.local_port || 18181}</div></div>
      <div class="status-card"><div class="label">内网账号</div><div class="value ${loginOnline ? 'online' : 'offline'}">${loginOnline ? '已配置' : '未配置'}</div><div class="subvalue">${state.status.login_account || state.status.login_label || '--'}</div></div>
      <div class="status-card"><div class="label">待审批</div><div class="value ${state.approvals.length > 0 ? 'pending' : ''}">${state.approvals.length}</div><div class="subvalue">超时：${state.settings?.timeout_seconds || 30}s</div></div>
    </div>
    <div class="card">
      <div class="card-header">常用入口</div>
      <div class="card-body">
        <div class="actions-row">
          <button class="btn btn-primary" data-action="open-dashboard">打开 Dashboard</button>
          <button class="btn" data-action="open-agent-directory">打开程序目录</button>
          <button class="btn" data-action="open-settings">设置</button>
        </div>
      </div>
    </div>
    ${state.approvals.length > 0 ? `<div class="card"><div class="card-header">待审批请求</div><div class="card-body">${renderApprovalItems()}</div></div>` : ''}
  `;
}
