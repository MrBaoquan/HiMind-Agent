function renderSettings() {
  if (!state.settings || !state.status || !state.loginState) return '<div class="empty"><div class="icon">&#8987;</div>加载中...</div>';
  return `
    <h2>设置</h2>
    ${renderMessage()}
    <div class="card">
      <div class="card-header">内网账号</div>
      <div class="card-body">
        <div class="login-compact">
          <div class="login-summary">
            <div>
              <div class="title">当前账号</div>
              <div class="account">${state.loginState.account || '未保存账号'}</div>
            </div>
            <span class="pill ${loginConfigured() ? 'success' : 'danger'}">${loginConfigured() ? '已配置' : '未配置'}</span>
          </div>
          <div class="setting-block">
            <div class="actions-row" style="margin-top:12px">
              <button class="btn btn-primary" data-action="open-login-modal">配置账号</button>
            </div>
          </div>
        </div>
      </div>
    </div>
    <div class="card">
      <div class="card-header">审批与启动</div>
      <div class="card-body">
        <div class="setting-group">
          <h3>远程连接</h3>
          <div class="setting-row"><div><div class="label-text">审批模式</div><div class="label-desc">收到远程连接请求时的处理方式</div></div><select data-action="update-rule" data-request-type="remote_connect"><option value="manual" ${state.settings.rules?.remote_connect === 'manual' ? 'selected' : ''}>手动审批</option><option value="auto_approve" ${state.settings.rules?.remote_connect === 'auto_approve' ? 'selected' : ''}>自动批准</option><option value="auto_deny" ${state.settings.rules?.remote_connect === 'auto_deny' ? 'selected' : ''}>自动拒绝</option></select></div>
        </div>
        <div class="setting-group">
          <h3>文件上传</h3>
          <div class="setting-row"><div><div class="label-text">审批模式</div><div class="label-desc">收到上传任务时的处理方式</div></div><select data-action="update-rule" data-request-type="upload_code"><option value="manual" ${state.settings.rules?.upload_code === 'manual' ? 'selected' : ''}>手动审批</option><option value="auto_approve" ${state.settings.rules?.upload_code === 'auto_approve' ? 'selected' : ''}>自动批准</option><option value="auto_deny" ${state.settings.rules?.upload_code === 'auto_deny' ? 'selected' : ''}>自动拒绝</option></select></div>
        </div>
        <div class="setting-group">
          <h3>启动</h3>
          <div class="setting-row"><div><div class="label-text">审批超时（秒）</div><div class="label-desc">未响应时自动拒绝的等待时间</div></div><select data-action="update-timeout"><option value="15" ${state.settings.timeout_seconds === 15 ? 'selected' : ''}>15 秒</option><option value="30" ${state.settings.timeout_seconds === 30 ? 'selected' : ''}>30 秒</option><option value="60" ${state.settings.timeout_seconds === 60 ? 'selected' : ''}>60 秒</option><option value="120" ${state.settings.timeout_seconds === 120 ? 'selected' : ''}>120 秒</option></select></div>
          <div class="setting-row"><div><div class="label-text">开机自启</div><div class="label-desc">Windows 登录后自动启动</div></div><label class="toggle"><input type="checkbox" ${state.settings.auto_start ? 'checked' : ''} data-action="update-auto-start" /><span class="slider"></span></label></div>
        </div>
      </div>
    </div>
  `;
}

function renderLoginModal() {
  if (!state.loginModalOpen) return '';
  return `
    <div class="modal-backdrop" data-action="close-login-modal">
      <div class="modal" data-modal="login-editor">
        <div class="modal-header">
          <h3>配置内网账号</h3>
          <button class="btn" data-action="close-login-modal">关闭</button>
        </div>
        <div class="modal-body">
          <div class="field-group">
            <label class="field-label">当前状态</label>
            <span class="pill ${loginConfigured() ? 'success' : 'danger'}">${loginConfigured() ? '已配置' : '未配置'}</span>
          </div>
          <div class="field-group">
            <label class="field-label">内网账号</label>
            <input value="${escapeHtml(state.drafts.loginUsername)}" data-field="login-username" placeholder="输入内网平台用户名" />
          </div>
          <div class="field-group">
            <label class="field-label">内网密码</label>
            <input type="password" value="${escapeHtml(state.drafts.loginPassword)}" data-field="login-password" placeholder="输入内网平台密码" />
          </div>
          <div class="modal-actions">
            <button class="btn btn-primary" data-action="save-login">保存到 Agent</button>
            <button class="btn" data-action="open-inner-admin">打开内网平台</button>
            ${loginConfigured() ? '<button class="btn btn-danger" data-action="logout-login">清除凭据</button>' : ''}
          </div>
        </div>
      </div>
    </div>
  `;
}
