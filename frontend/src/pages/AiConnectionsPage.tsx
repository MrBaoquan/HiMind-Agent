import { useState } from 'react';
import { BriefcaseBusiness, Check, CircleAlert, Copy, FolderOpen, Github, PlugZap, RefreshCw, Settings2, SquareTerminal, Trash2, Wrench, X } from 'lucide-react';
import { PageHeader, Pill } from '../components/Common';
import type { AiClientIntegration, AiIntegrationOverview, DashboardIdentityStatus, McpConnectionTestResult } from '../services/agentApi';

type AiConnectionsPageProps = {
  identity: DashboardIdentityStatus | null;
  integration: AiIntegrationOverview | null;
  testResult: McpConnectionTestResult | null;
  busyAction: string | null;
  onOpenAccount: () => void;
  onRefresh: () => void;
  onRegister: (clientId: string, resetInvalid?: boolean) => void;
  onUnregister: (clientId: string) => void;
  onOpenDirectory: (path: string) => void;
  onTest: () => void;
};

export function AiConnectionsPage({
  identity,
  integration,
  testResult,
  busyAction,
  onOpenAccount,
  onRefresh,
  onRegister,
  onUnregister,
  onOpenDirectory,
  onTest,
}: AiConnectionsPageProps) {
  const [copied, setCopied] = useState<string | null>(null);
  const [removeCandidate, setRemoveCandidate] = useState<string | null>(null);
  const [repairCandidate, setRepairCandidate] = useState<string | null>(null);
  const clients = integration?.clients || [];
  const installedCount = clients.filter(client => client.detected).length;
  const readyCount = clients.filter(client => client.detected && client.state === 'configured').length;
  const attentionCount = clients.filter(client => client.detected && (client.state === 'needs_repair' || client.state === 'invalid_config')).length;

  async function copyConfiguration(client: AiClientIntegration) {
    await navigator.clipboard.writeText(client.config_preview);
    setCopied(client.id);
    window.setTimeout(() => setCopied(current => current === client.id ? null : current), 1800);
  }

  return (
    <div className="ai-page">
      <PageHeader
        title="AI 连接"
        description="管理 HiMind MCP 服务在常用 AI 工具中的注册状态。"
        actions={<button className="btn btn-icon" title="刷新注册状态" aria-label="刷新注册状态" disabled={Boolean(busyAction)} onClick={onRefresh}><RefreshCw size={16} /></button>}
      />

      <div className="ai-connection-summary">
        <div className="ai-summary-icon"><PlugZap size={19} /></div>
        <div className="ai-summary-copy"><strong>MCP 服务注册状态</strong><span>注册后即可使用 HiMind 功能</span></div>
        <div className="ai-summary-metrics">
          <div><span>已注册</span><strong>{integration ? `${readyCount}/${installedCount}` : '—'}</strong></div>
          <div><span>需要处理</span><strong className={attentionCount ? 'warning-text' : ''}>{attentionCount}</strong></div>
        </div>
        <button className="btn" disabled={!integration || busyAction === 'test'} onClick={onTest}><PlugZap size={15} />{busyAction === 'test' ? '检查中' : '检查 MCP 服务'}</button>
      </div>

      {testResult ? (
        <div className="mcp-test-result">
          <Check size={16} />
          <span>MCP 服务正常</span>
          <strong>可以供已注册的 AI 工具使用</strong>
        </div>
      ) : null}

      {!identity?.authorized ? <div className="blocker account-blocker"><CircleAlert size={18} /><div><strong>尚未登录 HiMind</strong><span>注册 MCP 服务不受影响；使用工作台数据前需要登录。</span></div><button className="btn" onClick={onOpenAccount}>登录 HiMind</button></div> : null}

      <section className="ai-client-section">
        <div className="ai-section-heading">
          <div><h3>AI 工具</h3><span>注册或取消注册 HiMind MCP 服务</span></div>
          <Pill kind="neutral">已安装 {installedCount}/{clients.length || 3}</Pill>
        </div>
        <div className="ai-client-list">
          {clients.map(client => {
            const state = clientState(client);
            const pending = busyAction === `register:${client.id}` || busyAction === `unregister:${client.id}`;
            return (
              <article className="ai-client-row" key={client.id}>
                <div className={`ai-client-icon ${client.id}`}>{clientIcon(client.id)}</div>
                <div className="ai-client-copy"><strong>{client.name}</strong><span>{clientDescription(client)}</span></div>
                <Pill kind={state.kind}>{state.label}</Pill>
                <div className="ai-client-registration-actions">
                  {removeCandidate === client.id ? <div className="ai-remove-confirm"><button className="btn btn-danger-quiet" disabled={pending} onClick={() => { onUnregister(client.id); setRemoveCandidate(null); }}>确认取消注册</button><button className="btn btn-icon" title="保留注册" aria-label="保留 MCP 注册" onClick={() => setRemoveCandidate(null)}><X size={14} /></button></div> : client.state === 'invalid_config' && repairCandidate === client.id ? <div className="ai-repair-confirm"><small>将备份现有配置并重新注册 HiMind MCP 服务。</small><button className="btn btn-primary" disabled={pending} onClick={() => { onRegister(client.id, true); setRepairCandidate(null); }}>确认修复</button><button className="btn btn-icon" title="取消" aria-label="取消修复" onClick={() => setRepairCandidate(null)}><X size={14} /></button></div> : <>
                    {client.detected && client.state !== 'configured' ? <button className="btn btn-primary" disabled={pending} onClick={() => client.state === 'invalid_config' ? setRepairCandidate(client.id) : onRegister(client.id)}>{clientActionIcon(client)}{clientActionLabel(client)}</button> : null}
                    {client.state !== 'not_configured' ? <button className="btn btn-danger-quiet" disabled={pending} onClick={() => setRemoveCandidate(client.id)}><Trash2 size={14} />取消注册</button> : null}
                    {!client.detected && client.state === 'not_configured' ? <span>安装客户端后可注册</span> : null}
                  </>}
                </div>
              </article>
            );
          })}
        </div>
      </section>

      {clients.length ? <details className="ai-advanced">
        <summary><Settings2 size={16} /><span><strong>高级诊断</strong><small>查看客户端配置路径和原始配置</small></span></summary>
        <div className="ai-diagnostic-list">
          {clients.map(client => {
            const state = clientState(client);
            return <div className="ai-diagnostic-item" key={client.id}>
              <div className="ai-diagnostic-heading"><strong>{client.name}</strong><Pill kind={state.kind}>{state.label}</Pill></div>
              <div className="ai-diagnostic-path"><span>配置文件</span><code title={client.config_path}>{client.config_path}</code></div>
              {client.state === 'invalid_config' ? <div className="ai-diagnostic-error">原配置文件格式异常，需要备份后重建。</div> : null}
              <details className="ai-config-preview">
                <summary>查看原始配置</summary>
                <div className="ai-code-wrap"><pre>{client.config_preview}</pre><button className="btn btn-icon" title="复制配置" aria-label={`复制 ${client.name} 配置`} onClick={() => copyConfiguration(client)}>{copied === client.id ? <Check size={14} /> : <Copy size={14} />}</button></div>
              </details>
              <div className="ai-diagnostic-actions">
                <button className="btn btn-icon" title="打开配置目录" aria-label={`打开 ${client.name} 配置目录`} onClick={() => onOpenDirectory(client.config_directory)}><FolderOpen size={15} /></button>
              </div>
            </div>;
          })}
        </div>
      </details> : null}
    </div>
  );
}

function clientDescription(client: AiClientIntegration) {
  if (!client.detected && client.state !== 'not_configured') return '未检测到客户端，现有注册已保留';
  if (!client.detected) return '未在这台电脑上检测到客户端';
  if (client.state === 'configured') return `HiMind MCP 服务已注册，重启 ${client.name} 后生效`;
  if (client.state === 'needs_repair') return '当前注册需要更新';
  if (client.state === 'invalid_config') return '当前注册需要修复';
  return '可以注册 HiMind MCP 服务';
}

function clientActionLabel(client: AiClientIntegration) {
  if (client.state === 'needs_repair') return '更新注册';
  if (client.state === 'invalid_config') return '修复注册';
  return '注册 MCP 服务';
}

function clientActionIcon(client: AiClientIntegration) {
  if (client.state === 'needs_repair' || client.state === 'invalid_config') return <Wrench size={15} />;
  return <PlugZap size={15} />;
}

function clientIcon(clientId: string) {
  if (clientId === 'github-copilot') return <Github size={19} />;
  if (clientId === 'workbuddy') return <BriefcaseBusiness size={19} />;
  return <SquareTerminal size={19} />;
}

function clientState(client: AiClientIntegration): { label: string; kind: 'success' | 'warn' | 'danger' | 'neutral' } {
  if (!client.detected) return { label: '未安装', kind: 'neutral' };
  if (client.state === 'configured') return { label: '已注册', kind: 'success' };
  if (client.state === 'needs_repair') return { label: '注册需更新', kind: 'warn' };
  if (client.state === 'invalid_config') return { label: '注册异常', kind: 'danger' };
  return { label: '未注册', kind: 'neutral' };
}
