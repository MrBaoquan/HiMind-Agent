import { Check, CircleAlert, Globe, Pencil, PlugZap, Plus, RefreshCw, Store, Terminal, Trash2 } from 'lucide-react';
import { BusyIndicator } from './BusyIndicator';
import { Pill } from './Common';
import { McpConnectionForm } from './McpConnectionForm';
import { probeFailureText, type McpManager } from './useMcpManager';
import { formatCommand } from '../lib/commandLine';
import { transportLabel } from '../pages/mcpServerView';

/**
 * 已添加的工具连接：启停、编辑、删除、测试。市场与「我的能力」共用同一份数据，
 * 这里只负责画；没有的按钮不画，能点的按钮必须真的能用。
 */
export function McpConnectionsPanel({ mcp, onBrowseCatalog, note }: {
  mcp: McpManager;
  /** 空态里给的下一步；不传就只提示自定义连接。 */
  onBrowseCatalog?: () => void;
  /** 列表上方的说明；不同入口的措辞不一样（会话旁 / 我的能力）。 */
  note?: string;
}) {
  return (
    <>
      <McpConnectionForm mcp={mcp} />
      {mcp.draft ? null : (
        <section className="ai-client-section">
          <div className="ai-section-heading">
            <div>
              <h3>已添加</h3>
              <span>{note || '这些工具会在对话里提供给 HiMind AI 调用，停用后不再加载。'}</span>
            </div>
            <div className="mcp-heading-actions">
              {mcp.servers.length ? (
                <button type="button" className="btn" disabled={Boolean(mcp.busy) || mcp.probing} onClick={() => void mcp.probeServers(mcp.servers.map(item => item.server_name))}>
                  {mcp.probing ? <BusyIndicator size={14} /> : <PlugZap size={14} />}{mcp.probing ? '正在测试' : '测试连接'}
                </button>
              ) : null}
              <button type="button" className="btn" disabled={Boolean(mcp.busy)} onClick={mcp.beginAdd}><Plus size={14} />自定义连接</button>
            </div>
          </div>
          <div className="ai-client-list">
            {mcp.loading ? <div className="builtin-ai-mcp-message"><BusyIndicator size={16} />正在读取</div> : null}
            {!mcp.loading && !mcp.servers.length ? (
              <div className="builtin-ai-mcp-empty">
                <strong>还没有添加工具</strong>
              <span>日常问答无需配置；要读本机目录、访问内网接口或记住偏好，先添加一条连接。</span>
                {onBrowseCatalog ? <button type="button" className="btn" onClick={onBrowseCatalog}><Store size={14} />浏览 MCP 工具</button> : null}
              </div>
            ) : null}
            {mcp.servers.map(server => {
              const endpoint = server.transport === 'stdio' ? formatCommand(server.command, server.args) : server.url;
              const detail = `${endpoint} · ${transportLabel(server.transport)}`;
              return (
                <article className="ai-client-row" key={server.server_name}>
                  <div className="ai-client-icon code">{server.transport === 'stdio' ? <Terminal size={18} /> : <Globe size={18} />}</div>
                  <div className="ai-client-copy">
                    <strong>{server.display_name || server.server_name}</strong>
                    <span title={detail}>{detail}</span>
                    {probeLine(mcp, server.server_name)}
                  </div>
                  <Pill kind={server.enabled ? 'success' : 'neutral'}>{server.enabled ? '已启用' : '已停用'}</Pill>
                  <div className="ai-client-registration-actions">
                    {mcp.confirmDelete === server.server_name ? (
                      <span className="builtin-ai-mcp-confirm">
                        <button type="button" className="btn" onClick={() => mcp.setConfirmDelete('')}>取消</button>
                        <button type="button" className="btn btn-danger" disabled={Boolean(mcp.busy)} onClick={() => void mcp.removeServer(server.server_name)}>删除</button>
                      </span>
                    ) : (
                      <>
                        <label className="toggle compact" title={server.enabled ? '停用连接' : '启用连接'}><input type="checkbox" checked={server.enabled} disabled={Boolean(mcp.busy)} onChange={event => void mcp.setEnabled(server, event.target.checked)} /><span className="slider" /></label>
                        <button type="button" className="btn btn-icon" title="编辑连接" aria-label={`编辑 ${server.display_name || server.server_name}`} disabled={Boolean(mcp.busy)} onClick={() => mcp.beginEdit(server)}><Pencil size={15} /></button>
                        <button type="button" className="btn btn-icon ai-row-remove" title="删除连接" aria-label={`删除 ${server.display_name || server.server_name}`} disabled={Boolean(mcp.busy)} onClick={() => mcp.setConfirmDelete(server.server_name)}><Trash2 size={15} /></button>
                      </>
                    )}
                  </div>
                </article>
              );
            })}
          </div>
        </section>
      )}
      {mcp.error && !mcp.draft ? (
        <div className="builtin-ai-extension-feedback error" role="alert">
          <CircleAlert size={15} /><span>{mcp.error}</span>
          <button type="button" title="重新读取" aria-label="重新读取" onClick={() => void mcp.loadServers()}><RefreshCw size={14} /></button>
        </div>
      ) : null}
      {mcp.notice ? <div className="builtin-ai-extension-feedback success" role="status"><Check size={15} /><span>{mcp.notice}</span></div> : null}
    </>
  );
}

// 保存后自动测一次连接：这正是过去「加了连接没反应」的地方，必须给出真实结果。
// 失败时把「重试」放在这一行上，用户不用回到顶部再点一次批量测试。
function probeLine(mcp: McpManager, serverName: string) {
  const entry = mcp.probes[serverName];
  if (!entry) return null;
  if (entry.pending) return <span className="builtin-ai-mcp-probe" title="首次运行要先下载依赖，可能会慢一些"><BusyIndicator size={12} /><span>正在测试连接</span></span>;
  const { result } = entry;
  if (result.ok) {
    return <span className="builtin-ai-mcp-probe success" title={`${result.server_name || serverName} · ${result.transport} · ${result.protocol_version}`}><Check size={12} /><span>已连接 · {result.tool_count} 个工具 · {result.duration_ms} ms</span></span>;
  }
  const detail = result.error || '测试没有完成。';
  return (
    <span className="builtin-ai-mcp-probe error" title={`${result.error_kind ? `${result.error_kind}: ` : ''}${detail}`}>
      <CircleAlert size={12} />
      <span>{probeFailureText(result)}</span>
      <button type="button" className="builtin-ai-mcp-probe-retry" disabled={Boolean(mcp.busy)} onClick={() => void mcp.probeServers([serverName])}>重试</button>
    </span>
  );
}
