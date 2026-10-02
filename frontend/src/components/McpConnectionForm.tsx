import { ChevronRight, CircleAlert, Save } from 'lucide-react';
import { BusyIndicator } from './BusyIndicator';
import type { McpManager } from './useMcpManager';

/**
 * 手填一条 MCP 连接。只有启动命令是必填的，其余都能留空——
 * 这是「目录里没有、但我确实要接」的那条路，所以默认只铺必填项，高级设置折起来。
 */
export function McpConnectionForm({ mcp }: { mcp: McpManager }) {
  const draft = mcp.draft;
  if (!draft) return null;
  const stdio = draft.transport === 'stdio';
  return (
    <section className="builtin-ai-mcp-form-card" aria-label="工具连接配置">
      <div className="ai-section-heading">
        <div><h3>{mcp.editingName ? `编辑 ${draft.display_name || mcp.editingName}` : '自定义连接'}</h3><span>填好启动命令就能用，其余可以留空。</span></div>
      </div>
      <div className="builtin-ai-mcp-form-grid">
        <label className="field-group">
          <span className="field-label">名称</span>
          <input value={draft.display_name} placeholder="例如 项目知识库" onChange={event => mcp.setDraft({ ...draft, display_name: event.target.value })} />
        </label>
        <div className="field-group">
          <span className="field-label">连接方式</span>
          <div className="segmented-control">
            <button type="button" className={stdio ? 'active' : ''} onClick={() => mcp.setDraft({ ...draft, transport: 'stdio' })}>本地进程</button>
            <button type="button" className={!stdio ? 'active' : ''} onClick={() => mcp.setDraft({ ...draft, transport: 'streamable-http' })}>HTTP</button>
          </div>
        </div>
        {stdio ? (
          <label className="field-group builtin-ai-mcp-wide">
            <span className="field-label">启动命令</span>
            <input value={mcp.commandLine} spellCheck={false} placeholder="npx -y @modelcontextprotocol/server-memory@2026.8.31" onChange={event => mcp.setCommandLine(event.target.value)} />
            <span className="field-hint">和终端里一样，按空格切分；路径含空格时用双引号包起来。</span>
          </label>
        ) : (
          <label className="field-group builtin-ai-mcp-wide">
            <span className="field-label">服务地址</span>
            <input value={draft.url} placeholder="https://example.com/mcp" inputMode="url" spellCheck={false} onChange={event => mcp.setDraft({ ...draft, url: event.target.value })} />
          </label>
        )}
        <label className="field-group builtin-ai-mcp-wide">
          <span className="field-label">{stdio ? '环境变量' : '请求头'}</span>
          <textarea className="builtin-ai-mcp-map" value={mapText(stdio ? draft.env : draft.headers)} placeholder={stdio ? 'API_KEY=...' : 'Authorization=Bearer ...'} spellCheck={false} onChange={event => mcp.setDraft(stdio ? { ...draft, env: parseMap(event.target.value) } : { ...draft, headers: parseMap(event.target.value) })} />
          <span className="field-hint">每行一条 KEY=VALUE，不需要就留空。</span>
        </label>
        <details className="builtin-ai-mcp-advanced">
          <summary><ChevronRight size={14} />高级设置<small>服务 ID、工作目录、超时和重连</small></summary>
          <div className="builtin-ai-mcp-advanced-grid">
            <label className="field-group">
              <span className="field-label">服务 ID</span>
              <input value={draft.server_name} disabled={Boolean(mcp.editingName)} placeholder="留空则按名称生成" spellCheck={false} onChange={event => mcp.setDraft({ ...draft, server_name: event.target.value })} />
            </label>
            <label className="field-group">
              <span className="field-label">工具调用超时（秒）</span>
              <input type="number" min={1} max={600} value={Math.max(1, Math.round(draft.tool_call_timeout_ms / 1000))} onChange={event => mcp.setDraft({ ...draft, tool_call_timeout_ms: Math.max(1, Number(event.target.value) || 30) * 1000 })} />
            </label>
            {stdio ? (
              <label className="field-group">
                <span className="field-label">工作目录（可选）</span>
                <input value={draft.cwd} placeholder="留空则用会话目录" spellCheck={false} onChange={event => mcp.setDraft({ ...draft, cwd: event.target.value })} />
              </label>
            ) : null}
            <label className="builtin-ai-mcp-check">
              <input type="checkbox" checked={draft.reconnect} onChange={event => mcp.setDraft({ ...draft, reconnect: event.target.checked })} />
              <span><strong>断开后自动重连</strong><small>调用失败时自动重连并重试，适合长期运行的服务</small></span>
            </label>
            <label className="builtin-ai-mcp-check">
              <input type="checkbox" checked={draft.fail_on_startup_error} onChange={event => mcp.setDraft({ ...draft, fail_on_startup_error: event.target.checked })} />
              <span><strong>必须可用</strong><small>连不上时直接报错，不会静默少一套工具</small></span>
            </label>
          </div>
        </details>
      </div>
      {mcp.error ? <div className="blocker"><CircleAlert size={16} /><span>{mcp.error}</span></div> : null}
      <div className="builtin-ai-mcp-form-actions">
        <button type="button" className="btn" onClick={mcp.closeDraft}>取消</button>
        <button type="button" className="btn btn-primary" disabled={Boolean(mcp.busy)} onClick={() => void mcp.saveDraft()}>{mcp.busy === 'save' ? <BusyIndicator size={15} /> : <Save size={15} />}{mcp.busy === 'save' ? '正在保存' : '保存连接'}</button>
      </div>
    </section>
  );
}

function mapText(value: Record<string, string>) {
  return Object.entries(value).map(([key, item]) => `${key}=${item}`).join('\n');
}

function parseMap(value: string) {
  const result: Record<string, string> = {};
  for (const row of value.split(/\r?\n/)) {
    const separator = row.indexOf('=');
    if (separator <= 0) continue;
    const key = row.slice(0, separator).trim();
    if (key) result[key] = row.slice(separator + 1).trim();
  }
  return result;
}
