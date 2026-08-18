import { useCallback, useEffect, useState } from 'react';
import { Blocks, Bot, Check, ChevronDown, CircleAlert, Code2, LoaderCircle, Network, Plus, Puzzle, RefreshCw, Save, ShieldCheck, Trash2, Wrench, X } from 'lucide-react';
import { agentApi, type BuiltinAIMcpServer, type BuiltinAIToolContextSummary } from '../services/agentApi';
import { errorDetail } from '../types';

type ExtensionTab = 'mcp' | 'plugins' | 'skills';

type Props = {
  open: boolean;
  toolSummary: BuiltinAIToolContextSummary;
  onClose: () => void;
  onRuntimeChanged: () => void;
  onToolContextChanged: () => void;
  onOpenPlugins: () => void;
  onOpenSkills: () => void;
};

const emptyServer = (): BuiltinAIMcpServer => ({
  server_name: '',
  display_name: '',
  transport: 'stdio',
  command: '',
  args: [],
  env: {},
  cwd: '',
  url: '',
  headers: {},
  tool_call_timeout_ms: 30_000,
  fail_on_startup_error: false,
  reconnect: true,
  enabled: true,
});

export function BuiltinAiExtensionsDialog({ open, toolSummary, onClose, onRuntimeChanged, onToolContextChanged, onOpenPlugins, onOpenSkills }: Props) {
  const [tab, setTab] = useState<ExtensionTab>('mcp');
  const [servers, setServers] = useState<BuiltinAIMcpServer[]>([]);
  const [draft, setDraft] = useState<BuiltinAIMcpServer | null>(null);
  const [editingName, setEditingName] = useState('');
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState('');
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [confirmDelete, setConfirmDelete] = useState('');
  const [advancedOpen, setAdvancedOpen] = useState(false);

  const loadServers = useCallback(async () => {
    setLoading(true);
    setError('');
    try {
      setServers(await agentApi.builtinAiMcpServers());
    } catch (loadError) {
      setError(presentMcpError(loadError, '无法读取 MCP 连接。'));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!open) return;
    setConfirmDelete('');
    setNotice('');
    void loadServers();
  }, [loadServers, open]);

  if (!open) return null;

  function beginAdd() {
    setDraft(emptyServer());
    setEditingName('');
    setAdvancedOpen(false);
    setError('');
    setNotice('');
  }

  function beginEdit(server: BuiltinAIMcpServer) {
    setDraft({ ...server, args: [...server.args], env: { ...server.env }, headers: { ...server.headers } });
    setEditingName(server.server_name);
    setAdvancedOpen(false);
    setError('');
    setNotice('');
  }

  async function saveDraft() {
    if (!draft || busy) return;
    setBusy('save');
    setError('');
    setNotice('');
    try {
      await agentApi.validateBuiltinAiMcpServer(draft);
      const saved = await agentApi.saveBuiltinAiMcpServer(draft);
      await loadServers();
      setDraft({ ...saved, args: [...saved.args], env: { ...saved.env }, headers: { ...saved.headers } });
      setEditingName(saved.server_name);
      setNotice('已保存并重新连接 HiMind AI。');
      onToolContextChanged();
      onRuntimeChanged();
    } catch (saveError) {
      setError(presentMcpError(saveError, 'MCP 连接保存失败。'));
    } finally {
      setBusy('');
    }
  }

  async function setEnabled(server: BuiltinAIMcpServer, enabled: boolean) {
    if (busy) return;
    setBusy(`toggle:${server.server_name}`);
    setError('');
    setNotice('');
    try {
      await agentApi.saveBuiltinAiMcpServer({ ...server, enabled });
      await loadServers();
      setNotice(enabled ? 'MCP 连接已启用。' : 'MCP 连接已停用。');
      onToolContextChanged();
      onRuntimeChanged();
    } catch (toggleError) {
      setError(presentMcpError(toggleError, '无法更新 MCP 连接。'));
    } finally {
      setBusy('');
    }
  }

  async function removeServer(serverName: string) {
    if (busy) return;
    setBusy(`delete:${serverName}`);
    setError('');
    setNotice('');
    try {
      await agentApi.deleteBuiltinAiMcpServer(serverName);
      if (editingName === serverName) {
        setDraft(null);
        setEditingName('');
      }
      setConfirmDelete('');
      await loadServers();
      setNotice('MCP 连接已删除。');
      onToolContextChanged();
      onRuntimeChanged();
    } catch (removeError) {
      setError(presentMcpError(removeError, '无法删除 MCP 连接。'));
    } finally {
      setBusy('');
    }
  }

  return (
    <div className="modal-backdrop builtin-ai-extension-backdrop" role="presentation" onMouseDown={event => { if (event.currentTarget === event.target) onClose(); }}>
      <section className="builtin-ai-extension-dialog" role="dialog" aria-modal="true" aria-labelledby="builtin-ai-extension-title">
        <header className="builtin-ai-extension-header">
          <div><span className="builtin-ai-extension-mark"><Blocks size={18} /></span><div><h3 id="builtin-ai-extension-title">扩展</h3><p>管理 HiMind AI 可用的连接与能力</p></div></div>
          <button type="button" className="btn btn-icon" title="关闭" aria-label="关闭" onClick={onClose}><X size={16} /></button>
        </header>
        <div className="builtin-ai-extension-tabs" role="tablist" aria-label="扩展类型">
          <button type="button" role="tab" aria-selected={tab === 'mcp'} className={tab === 'mcp' ? 'active' : ''} onClick={() => setTab('mcp')}><Network size={15} />MCP <span>{servers.length + 1}</span></button>
          <button type="button" role="tab" aria-selected={tab === 'plugins'} className={tab === 'plugins' ? 'active' : ''} onClick={() => setTab('plugins')}><Puzzle size={15} />插件</button>
          <button type="button" role="tab" aria-selected={tab === 'skills'} className={tab === 'skills' ? 'active' : ''} onClick={() => setTab('skills')}><Wrench size={15} />技能 <span>{toolSummary.skills}</span></button>
        </div>

        {tab === 'mcp' ? (
          <div className="builtin-ai-mcp-layout">
            <section className="builtin-ai-mcp-list" aria-label="MCP 连接">
              <div className="builtin-ai-mcp-list-head"><div><strong>MCP 连接</strong><span>为对话添加外部工具</span></div><button type="button" className="btn btn-icon" title="添加 MCP 连接" aria-label="添加 MCP 连接" onClick={beginAdd}><Plus size={16} /></button></div>
              <div className="builtin-ai-mcp-rows">
                <div className="builtin-ai-mcp-row builtin-ai-mcp-managed"><span className="builtin-ai-mcp-icon"><ShieldCheck size={16} /></span><button type="button" disabled><strong>HiMind</strong><small>内置服务</small></button><span className="builtin-ai-mcp-state">常驻</span></div>
                {loading ? <div className="builtin-ai-mcp-message"><LoaderCircle className="spin" size={16} />正在读取</div> : null}
                {!loading && !servers.length ? <div className="builtin-ai-mcp-message">还没有个人 MCP 连接</div> : null}
                {servers.map(server => (
                  <div className={`builtin-ai-mcp-row ${editingName === server.server_name ? 'selected' : ''}`} key={server.server_name}>
                    <span className="builtin-ai-mcp-icon"><Code2 size={16} /></span>
                    <button type="button" onClick={() => beginEdit(server)}><strong>{server.display_name || server.server_name}</strong><small>{server.server_name} · {server.transport === 'stdio' ? '本地进程' : 'HTTP'}</small></button>
                    {confirmDelete === server.server_name ? <span className="builtin-ai-mcp-confirm"><button type="button" onClick={() => setConfirmDelete('')}>取消</button><button type="button" className="danger-text" disabled={Boolean(busy)} onClick={() => void removeServer(server.server_name)}>删除</button></span> : <>
                      <label className="toggle compact" title={server.enabled ? '停用连接' : '启用连接'}><input type="checkbox" checked={server.enabled} disabled={Boolean(busy)} onChange={event => void setEnabled(server, event.target.checked)} /><span className="slider" /></label>
                      <button type="button" className="builtin-ai-mcp-delete" title="删除" aria-label={`删除 ${server.display_name || server.server_name}`} onClick={() => setConfirmDelete(server.server_name)}><Trash2 size={14} /></button>
                    </>}
                  </div>
                ))}
              </div>
            </section>

            <section className="builtin-ai-mcp-editor" aria-label="MCP 连接设置">
              {!draft ? <div className="builtin-ai-mcp-empty"><Network size={24} /><strong>选择或添加 MCP 连接</strong><span>内置 HiMind 服务由系统维护，无需配置。</span><button type="button" className="btn btn-primary" onClick={beginAdd}><Plus size={15} />添加连接</button></div> : <>
                <div className="builtin-ai-mcp-editor-head"><div><strong>{editingName ? '编辑 MCP 连接' : '添加 MCP 连接'}</strong><span>{editingName ? editingName : '填写连接信息后即可使用'}</span></div><label className="toggle"><input type="checkbox" checked={draft.enabled} onChange={event => setDraft({ ...draft, enabled: event.target.checked })} /><span className="slider" /></label></div>
                <div className="builtin-ai-mcp-form">
                  <div className="builtin-ai-mcp-two-columns">
                    <label><span>显示名称</span><input value={draft.display_name} placeholder="例如：项目知识库" onChange={event => setDraft({ ...draft, display_name: event.target.value })} /></label>
                    <label><span>服务 ID</span><input value={draft.server_name} disabled={Boolean(editingName)} placeholder="project-kb" spellCheck={false} onChange={event => setDraft({ ...draft, server_name: event.target.value })} /></label>
                  </div>
                  <fieldset><legend>连接方式</legend><div className="segmented-control"><button type="button" className={draft.transport === 'stdio' ? 'active' : ''} onClick={() => setDraft({ ...draft, transport: 'stdio' })}>本地进程</button><button type="button" className={draft.transport === 'streamable-http' ? 'active' : ''} onClick={() => setDraft({ ...draft, transport: 'streamable-http' })}>HTTP</button></div></fieldset>
                  {draft.transport === 'stdio' ? <>
                    <label><span>启动命令</span><input value={draft.command} placeholder="例如：npx" spellCheck={false} onChange={event => setDraft({ ...draft, command: event.target.value })} /></label>
                    <label><span>启动参数 <small>每行一项</small></span><textarea value={draft.args.join('\n')} placeholder={'-y\n@company/mcp-server'} spellCheck={false} onChange={event => setDraft({ ...draft, args: lines(event.target.value) })} /></label>
                  </> : <label><span>服务地址</span><input value={draft.url} placeholder="https://example.com/mcp" inputMode="url" spellCheck={false} onChange={event => setDraft({ ...draft, url: event.target.value })} /></label>}
                  <label><span>{draft.transport === 'stdio' ? '环境变量' : '请求头'} <small>每行 KEY=VALUE</small></span><textarea value={mapText(draft.transport === 'stdio' ? draft.env : draft.headers)} placeholder={draft.transport === 'stdio' ? 'API_KEY=...' : 'Authorization=Bearer ...'} spellCheck={false} onChange={event => setDraft(draft.transport === 'stdio' ? { ...draft, env: parseMap(event.target.value) } : { ...draft, headers: parseMap(event.target.value) })} /></label>
                  <button type="button" className="builtin-ai-mcp-advanced-toggle" aria-expanded={advancedOpen} onClick={() => setAdvancedOpen(openValue => !openValue)}><ChevronDown className={advancedOpen ? 'open' : ''} size={15} />高级设置</button>
                  {advancedOpen ? <div className="builtin-ai-mcp-advanced">
                    {draft.transport === 'stdio' ? <label><span>工作目录</span><input value={draft.cwd} placeholder="可选" spellCheck={false} onChange={event => setDraft({ ...draft, cwd: event.target.value })} /></label> : null}
                    <label><span>工具调用超时（秒）</span><input type="number" min={1} max={600} value={Math.max(1, Math.round(draft.tool_call_timeout_ms / 1000))} onChange={event => setDraft({ ...draft, tool_call_timeout_ms: Math.max(1, Number(event.target.value) || 30) * 1000 })} /></label>
                    <label className="builtin-ai-mcp-check"><input type="checkbox" checked={draft.reconnect} onChange={event => setDraft({ ...draft, reconnect: event.target.checked })} /><span><strong>断开后自动重连</strong><small>适合长期运行的服务</small></span></label>
                    <label className="builtin-ai-mcp-check"><input type="checkbox" checked={draft.fail_on_startup_error} onChange={event => setDraft({ ...draft, fail_on_startup_error: event.target.checked })} /><span><strong>连接失败时阻止会话启动</strong><small>仅用于必须可用的服务</small></span></label>
                  </div> : null}
                </div>
                <footer className="builtin-ai-mcp-editor-actions"><button type="button" className="btn" onClick={() => { setDraft(null); setEditingName(''); setError(''); }}>取消</button><button type="button" className="btn btn-primary" disabled={Boolean(busy)} onClick={() => void saveDraft()}>{busy === 'save' ? <LoaderCircle className="spin" size={15} /> : <Save size={15} />}{busy === 'save' ? '正在保存' : '保存连接'}</button></footer>
              </>}
            </section>
          </div>
        ) : null}

        {tab === 'plugins' ? <div className="builtin-ai-extension-overview"><span className="builtin-ai-extension-overview-icon"><Puzzle size={22} /></span><div><h4>AI 对话插件</h4><p>内置对话插件可在会话左下角的“设置”中配置，插件清单会随 HiMind AI 运行时更新。</p><div className="builtin-ai-extension-facts"><span><Check size={14} />保留原生插件设置</span><span><Check size={14} />个人扩展不受组织清单限制</span></div></div><div className="builtin-ai-extension-action"><strong>Agent 插件</strong><span>安装、停用或卸载本机能力插件</span><button type="button" className="btn" onClick={onOpenPlugins}><Puzzle size={15} />管理 Agent 插件</button></div></div> : null}
        {tab === 'skills' ? <div className="builtin-ai-extension-overview"><span className="builtin-ai-extension-overview-icon"><Bot size={22} /></span><div><h4>技能</h4><p>HiMind AI 会使用本机已安装的技能，也支持当前工作区提供的技能。</p><div className="builtin-ai-extension-facts"><span><Check size={14} />已安装 {toolSummary.skills} 项</span><span><Check size={14} />组织能力在调用时校验权限</span></div></div><div className="builtin-ai-extension-action"><strong>技能管理</strong><span>浏览、安装或更新技能</span><button type="button" className="btn" onClick={onOpenSkills}><Wrench size={15} />打开技能</button></div></div> : null}

        {error ? <div className="builtin-ai-extension-feedback error" role="alert"><CircleAlert size={15} /><span>{error}</span><button type="button" title="重新读取" aria-label="重新读取" onClick={() => void loadServers()}><RefreshCw size={14} /></button></div> : null}
        {notice ? <div className="builtin-ai-extension-feedback success" role="status"><Check size={15} /><span>{notice}</span></div> : null}
      </section>
    </div>
  );
}

function lines(value: string) {
  return value.split(/\r?\n/).map(item => item.trim()).filter(Boolean);
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

function presentMcpError(error: unknown, fallback: string) {
  const detail = errorDetail(error);
  if (!detail || detail.toLowerCase().includes('invoke') || detail.toLowerCase().includes('undefined')) return fallback;
  return detail;
}
