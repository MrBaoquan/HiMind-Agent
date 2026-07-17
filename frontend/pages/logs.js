function renderLogs() {
  if (state.logs.length === 0) return '<h2>运行日志</h2><div class="card"><div class="card-body"><div class="empty"><div class="icon">&#9776;</div>暂无日志记录</div></div></div>';
  return `
    <h2>运行日志</h2>
    ${renderMessage()}
    <div class="card">
      <div class="card-header">最近日志 <button class="btn" data-action="refresh-logs">刷新</button></div>
      <div class="card-body log-list">
        ${state.logs.slice(-120).reverse().map(l => `<div class="log-entry"><span class="time">${l.time || ''}</span><span class="level ${l.level || 'info'}">${(l.level || 'info').toUpperCase()}</span><span class="msg">${l.message || ''}</span></div>`).join('')}
      </div>
    </div>
  `;
}
