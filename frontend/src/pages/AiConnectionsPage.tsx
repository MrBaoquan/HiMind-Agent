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
  onConfigure: (clientId: string, resetInvalid?: boolean) => void;
  onRemove: (clientId: string) => void;
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
  onConfigure,
  onRemove,
  onOpenDirectory,
  onTest,
}: AiConnectionsPageProps) {
  const [copied, setCopied] = useState<string | null>(null);
  const [removeCandidate, setRemoveCandidate] = useState<string | null>(null);
  const [repairCandidate, setRepairCandidate] = useState<string | null>(null);

  async function copyConfiguration(client: AiClientIntegration) {
    await navigator.clipboard.writeText(client.config_preview);
    setCopied(client.id);
    window.setTimeout(() => setCopied(current => current === client.id ? null : current), 1800);
  }

  return (
    <div className="ai-page">
      <PageHeader
        title="连接 AI"
        description="让常用 AI 工具使用 HiMind 提供的功能。"
        actions={<button className="btn btn-icon" title="刷新连接状态" aria-label="刷新连接状态" disabled={Boolean(busyAction)} onClick={onRefresh}><RefreshCw size={16} /></button>}
      />

      <div className="ai-runtime-strip">
        <div><span className="status-dot success" /><span>本机服务</span><strong>已准备</strong></div>
        <div><span>已写入配置</span><strong>{integration?.clients.filter(client => client.state === 'configured').length || 0}/{integration?.clients.length || 3}</strong></div>
        <div className="ai-runtime-technical" title={`${integration?.protocol || 'MCP stdio'} · ${integration?.server_id || 'himind-agent'}`}><span>连接方式</span><strong>本机连接</strong></div>
        <button className="btn" disabled={!integration || busyAction === 'test'} onClick={onTest}><PlugZap size={15} />{busyAction === 'test' ? '检查中' : '检查本机服务'}</button>
      </div>

      {testResult ? (
        <div className="mcp-test-result">
          <Check size={16} />
          <span>本机服务正常</span>
          <strong>{testResult.capability_count} 项功能可用</strong>
          <code>{testResult.duration_ms} ms</code>
        </div>
      ) : null}

      {!identity?.authorized ? <div className="blocker account-blocker"><CircleAlert size={18} /><div><strong>尚未登录 HiMind</strong><span>连接 AI 不受影响；使用工作台数据前需要登录。</span></div><button className="btn" onClick={onOpenAccount}>前往登录</button></div> : null}

      <section className="ai-client-section">
        <div className="ai-section-heading">
          <div><h3>AI 工具连接</h3><span>选择要使用的客户端，连接信息会保存到本机</span></div>
          <Pill kind="neutral">支持 {integration?.clients.length || 3} 个</Pill>
        </div>
        <div className="ai-client-grid">
          {(integration?.clients || []).map(client => {
            const state = clientState(client);
            const pending = busyAction === `configure:${client.id}` || busyAction === `remove:${client.id}`;
            return (
              <article className="ai-client-card" key={client.id}>
                <div className="ai-client-header">
                  <div className={`ai-client-icon ${client.id}`}>{clientIcon(client.id)}</div>
                  <div><strong>{client.name}</strong><span>{client.detection_message}</span></div>
                  <div className="ai-client-state"><span className={`status-dot ${state.kind === 'success' ? 'success' : state.kind === 'danger' ? 'danger' : ''}`} /><Pill kind={state.kind}>{state.label}</Pill></div>
                </div>
                <div className="ai-client-config">
                  <span>连接信息保存位置</span>
                  <code title={client.config_path}>{client.config_path}</code>
                  {client.state === 'invalid_config' ? <small>原配置文件格式有误，需要备份后重建</small> : null}
                </div>
                <details className="ai-config-preview">
                  <summary>查看配置</summary>
                  <div className="ai-code-wrap">
                    <pre>{client.config_preview}</pre>
                    <button className="btn btn-icon" title="复制配置" aria-label={`复制 ${client.name} 配置`} onClick={() => copyConfiguration(client)}>{copied === client.id ? <Check size={14} /> : <Copy size={14} />}</button>
                  </div>
                </details>
                {client.state === 'configured' ? <div className="ai-client-next-step"><Check size={14} /><span>连接信息已准备好，重启 {client.name} 后即可使用</span></div> : null}
                <div className="ai-client-actions">
                  {client.state === 'invalid_config' && repairCandidate === client.id ? (
                    <div className="ai-repair-confirm">
                      <span>继续后会备份原文件，并只保留新的 HiMind 连接信息。</span>
                      <button className="btn btn-primary" disabled={pending} onClick={() => { onConfigure(client.id, true); setRepairCandidate(null); }}>备份并重建</button>
                      <button className="btn btn-icon" title="取消" aria-label="取消重建" onClick={() => setRepairCandidate(null)}><X size={14} /></button>
                    </div>
                  ) : (
                    <button
                      className="btn btn-primary"
                      disabled={pending || (!client.detected && client.state === 'not_configured')}
                      title={!client.detected && client.state === 'not_configured' ? `请先安装 ${client.name}` : undefined}
                      onClick={() => client.state === 'invalid_config' ? setRepairCandidate(client.id) : onConfigure(client.id)}
                    >
                      {client.state === 'configured' ? <Settings2 size={15} /> : client.state === 'needs_repair' || client.state === 'invalid_config' ? <Wrench size={15} /> : <PlugZap size={15} />}
                      {client.state === 'configured' ? '更新连接' : client.state === 'invalid_config' ? '重建连接' : client.state === 'needs_repair' ? '更新连接' : '连接'}
                    </button>
                  )}
                  <button className="btn btn-icon" title="打开连接信息目录" aria-label={`打开 ${client.name} 连接信息目录`} onClick={() => onOpenDirectory(client.config_directory)}><FolderOpen size={15} /></button>
                  {client.state === 'configured' || client.state === 'needs_repair' ? (
                    removeCandidate === client.id ? (
                      <div className="ai-remove-confirm"><button className="btn btn-danger-quiet" disabled={pending} onClick={() => { onRemove(client.id); setRemoveCandidate(null); }}>确认断开</button><button className="btn btn-icon" title="取消" aria-label="取消断开" onClick={() => setRemoveCandidate(null)}><X size={14} /></button></div>
                    ) : <button className="btn btn-icon ai-remove" title="断开连接" aria-label={`断开 ${client.name}`} onClick={() => setRemoveCandidate(client.id)}><Trash2 size={15} /></button>
                  ) : null}
                </div>
              </article>
            );
          })}
        </div>
      </section>
    </div>
  );
}

function clientIcon(clientId: string) {
  if (clientId === 'github-copilot') return <Github size={19} />;
  if (clientId === 'workbuddy') return <BriefcaseBusiness size={19} />;
  return <SquareTerminal size={19} />;
}

function clientState(client: AiClientIntegration): { label: string; kind: 'success' | 'warn' | 'danger' | 'neutral' } {
  if (client.state === 'configured') return { label: '已连接', kind: 'success' };
  if (client.state === 'needs_repair') return { label: '需要更新', kind: 'warn' };
  if (client.state === 'invalid_config') return { label: '需要重建', kind: 'danger' };
  return { label: client.detected ? '未连接' : '未安装', kind: client.detected ? 'warn' : 'neutral' };
}
