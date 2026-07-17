function renderApprovals() {
  return `
    <h2>审批管理</h2>
    ${renderMessage()}
    <div class="page-note">远控连接和上传任务的默认处理方式由 Agent 本机决定，不再依赖 Dashboard 页面临时确认。</div>
    <div class="card">
      <div class="card-header">待处理请求 <button class="btn" data-action="refresh-approvals">刷新</button></div>
      <div class="card-body">
        ${state.approvals.length === 0 ? '<div class="empty"><div class="icon">&#10003;</div>暂无待审批请求</div>' : renderApprovalItems()}
      </div>
    </div>
  `;
}

function renderApprovalItems() {
  return state.approvals.map(a => `
    <div class="approval-item">
      <div class="icon ${a.request_type}">${a.request_type === 'remote_connect' ? '&#9742;' : '&#8682;'}</div>
      <div class="info"><div class="title">${a.title}</div><div class="desc">${a.description}</div></div>
      <span class="timer">${formatTimeout(a.timeout_seconds)}s</span>
      <div class="actions">
        <button class="btn btn-success" data-action="respond-approval" data-id="${a.id}" data-approved="true">允许</button>
        <button class="btn btn-danger" data-action="respond-approval" data-id="${a.id}" data-approved="false">拒绝</button>
      </div>
    </div>
  `).join('');
}
