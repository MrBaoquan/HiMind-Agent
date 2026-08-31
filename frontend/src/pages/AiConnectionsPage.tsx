import { useEffect, useMemo, useState, type ReactNode } from 'react';
import {
  Activity,
  Check,
  CircleAlert,
  CircleCheck,
  CircleDashed,
  Code2,
  Copy,
  FolderOpen,
  Github,
  MonitorDot,
  Link2Off,
  PlugZap,
  RefreshCw,
  Settings2,
  ShieldCheck,
  Unplug,
} from 'lucide-react';
import { PageHeader, Pill } from '../components/Common';
import { AiServicesPanel } from './AiServicesPanel';
import type {
  AIServiceListResult,
  CustomAIService,
  DashboardIdentityStatus,
  McpConnectionTestResult,
  McpTargetDescriptor,
} from '../services/agentApi';

type AiConnectionsPageProps = {
  initialTab?: 'mcp' | 'services';
  identity: DashboardIdentityStatus | null;
  dashboardEnabled: boolean;
  targets: McpTargetDescriptor[];
  testResult: McpConnectionTestResult | null;
  busyAction: string | null;
  aiServices: AIServiceListResult | null;
  onOpenAccount: () => void;
  onRefresh: () => void;
  onApplyTarget: (targetId: string, resetInvalid?: boolean) => void;
  onApplyAll: () => void;
  onRemoveAll: () => void;
  onRemoveTarget: (targetId: string) => void;
  onOpenDirectory: (path: string) => void;
  onTest: () => void;
  onSaveAIService: (input: {
    id: string;
    display_name: string;
    base_url: string;
    protocol: 'openai-chat' | 'openai-responses';
    model: string;
    models: string[];
    api_key: string;
  }) => Promise<void>;
  onRemoveAIService: (id: string) => Promise<void>;
  onImportAIClient: (target: string, service?: string) => Promise<void>;
  onRemoveAIClient: (target: string) => Promise<void>;
  onFetchModels: (input: { base_url: string; api_key: string }) => Promise<string[]>;
  onFetchSavedModels: (id: string, base_url: string) => Promise<string[]>;
};

export function AiConnectionsPage({
  initialTab = 'mcp',
  identity,
  dashboardEnabled,
  targets,
  testResult,
  busyAction,
  aiServices,
  onOpenAccount,
  onRefresh,
  onApplyTarget,
  onApplyAll,
  onRemoveAll,
  onRemoveTarget,
  onOpenDirectory,
  onTest,
  onSaveAIService,
  onRemoveAIService,
  onImportAIClient,
  onRemoveAIClient,
  onFetchModels,
  onFetchSavedModels,
}: AiConnectionsPageProps) {
  const [activeTab, setActiveTab] = useState<'mcp' | 'services'>(initialTab);
  const [copied, setCopied] = useState<string | null>(null);

  useEffect(() => {
    setActiveTab(initialTab);
  }, [initialTab]);

  const groups = useMemo(() => {
    const ordered = [...targets].sort((left, right) => {
      if (left.id === 'himind-ai') return -1;
      if (right.id === 'himind-ai') return 1;
      return left.name.localeCompare(right.name);
    });
    const builtin = ordered.filter(target => target.id === 'himind-ai');
    const external = ordered.filter(target => target.id !== 'himind-ai');
    return {
      builtin,
      connected: external.filter(target => target.state === 'configured'),
      actionable: external.filter(target => target.detected && target.state !== 'configured' && target.supports_auto_configure),
      manual: external.filter(target => target.detected && target.state !== 'configured' && !target.supports_auto_configure),
      unavailable: external.filter(target => !target.detected && target.state !== 'configured'),
    };
  }, [targets]);

  const readyCount = groups.builtin.length + groups.connected.length;
  const discoveredCount = groups.builtin.length + groups.connected.length + groups.actionable.length + groups.manual.length;
  const attentionCount = groups.actionable.length + groups.manual.length;
  const headline = attentionCount ? `${attentionCount} 个 MCP 注册待处理` : 'AI 工具已就绪';
  const headlineDescription = attentionCount
    ? '完成注册后，已发现的 AI 工具即可使用 Agent 的统一能力。'
    : '本机已发现的 AI 工具都可以使用 Agent 的统一能力。';

  async function copyConfiguration(target: McpTargetDescriptor) {
    const content = target.config_preview || target.manual_snippet;
    if (!content) return;
    await navigator.clipboard.writeText(content);
    setCopied(target.id);
    window.setTimeout(() => setCopied(current => current === target.id ? null : current), 1800);
  }

  return (
    <div className="ai-page">
      <PageHeader
        title="AI 连接"
        description="管理 AI 客户端与 HiMind Agent 之间的双向接入。"
        actions={<button className="btn btn-icon" title="刷新状态" aria-label="刷新状态" disabled={Boolean(busyAction)} onClick={onRefresh}><RefreshCw size={16} /></button>}
      />

      <div className="ai-tabs" role="tablist" aria-label="AI 连接分类">
        <button type="button" id="ai-tab-mcp" role="tab" aria-selected={activeTab === 'mcp'} aria-controls="ai-panel-mcp" className={`ai-tab${activeTab === 'mcp' ? ' active' : ''}`} onClick={() => setActiveTab('mcp')}>MCP 注册</button>
        <button type="button" id="ai-tab-services" role="tab" aria-selected={activeTab === 'services'} aria-controls="ai-panel-services" className={`ai-tab${activeTab === 'services' ? ' active' : ''}`} onClick={() => setActiveTab('services')}>AI 服务</button>
      </div>

      {activeTab === 'services' ? (
        <div id="ai-panel-services" role="tabpanel" aria-labelledby="ai-tab-services">
          <AiServicesPanel
            aiServices={aiServices}
            onRefresh={onRefresh}
            onSaveAIService={onSaveAIService}
            onRemoveAIService={onRemoveAIService}
            onImportAIClient={onImportAIClient}
            onRemoveAIClient={onRemoveAIClient}
            onOpenAccount={onOpenAccount}
            onFetchModels={onFetchModels}
            onFetchSavedModels={onFetchSavedModels}
          />
        </div>
      ) : (
        <div id="ai-panel-mcp" role="tabpanel" aria-labelledby="ai-tab-mcp">
          <section className={`ai-overview ${attentionCount ? 'attention' : 'ready'}`}>
            <div className="ai-overview-main">
              <div className="ai-overview-icon">{attentionCount ? <CircleAlert size={20} /> : <ShieldCheck size={20} />}</div>
              <div className="ai-overview-copy">
                <span className="ai-overview-eyebrow">MCP 注册</span>
                <strong>{headline}</strong>
                <span>{headlineDescription}</span>
              </div>
            </div>
            <div className="ai-overview-stats" aria-label="MCP 注册统计">
              <div><span>已就绪</span><strong>{readyCount}</strong></div>
              <div><span>待处理</span><strong className={attentionCount ? 'warning-text' : ''}>{attentionCount}</strong></div>
              <div><span>已发现</span><strong>{discoveredCount}</strong></div>
            </div>
            <div className="ai-overview-actions">
              <button className="btn btn-primary" disabled={Boolean(busyAction) || !groups.actionable.length} onClick={onApplyAll}>
                <PlugZap size={15} />{busyAction === 'apply-all' ? '正在注册' : groups.actionable.length ? '一键注册 MCP' : groups.manual.length ? '需要手动配置' : '注册已完成'}
              </button>
              <button className="btn btn-icon ai-overview-remove" title="取消全部注册" aria-label="取消全部注册" disabled={Boolean(busyAction) || !groups.connected.length} onClick={onRemoveAll}><Link2Off size={16} /></button>
              <button className="btn btn-icon" title="检查 Agent MCP 服务" aria-label="检查 Agent MCP 服务" disabled={Boolean(busyAction)} onClick={onTest}>
                <Activity size={16} />
              </button>
            </div>
          </section>

          {testResult ? (
            <div className={`mcp-test-result ${testResult.ok ? 'success' : 'error'}`} role="status">
              <div className="mcp-test-icon">{testResult.ok ? <CircleCheck size={17} /> : <CircleAlert size={17} />}</div>
              <div className="mcp-test-copy">
                <strong>{testResult.ok ? 'Agent MCP 服务正常' : 'Agent MCP 服务异常'}</strong>
                <span>{testResult.server_name || 'himind-agent'} · 协议 {testResult.protocol_version || '--'}</span>
              </div>
              <div className="mcp-test-metrics">
                <div><span>工具</span><strong>{testResult.capability_count}</strong></div>
                <div><span>耗时</span><strong>{testResult.duration_ms} ms</strong></div>
              </div>
            </div>
          ) : null}

          {dashboardEnabled && !identity?.authorized ? <div className="blocker account-blocker"><CircleAlert size={18} /><div><strong>工作台账号未连接</strong><span>MCP 注册不受影响；需要访问工作台数据时再完成账号连接。</span></div><button className="btn" onClick={onOpenAccount}>连接账号</button></div> : null}

          {groups.builtin.length ? <ConnectionSection title="Agent 内置" description="HiMind AI 会话自动加载本地 MCP、技能和插件能力。" count={groups.builtin.length}>
            {groups.builtin.map(target => <ConnectionRow key={target.id} target={target} busyAction={busyAction} onApplyTarget={onApplyTarget} onRemoveTarget={onRemoveTarget} onCopyTarget={copyConfiguration} onOpenDirectory={onOpenDirectory} />)}
          </ConnectionSection> : null}

          <ConnectionSection title="已注册 MCP" description="这些 AI 工具已经可以调用 HiMind Agent。" count={groups.connected.length}>
            {groups.connected.map(target => <ConnectionRow key={target.id} target={target} busyAction={busyAction} onApplyTarget={onApplyTarget} onRemoveTarget={onRemoveTarget} onCopyTarget={copyConfiguration} onOpenDirectory={onOpenDirectory} />)}
            {!groups.connected.length ? <EmptyConnectionRow text="还没有注册 MCP 的外部 AI 工具" /> : null}
          </ConnectionSection>

          <ConnectionSection title="待注册" description="已在本机发现，可一键写入 MCP 配置。" count={groups.actionable.length} tone={groups.actionable.length ? 'attention' : 'default'}>
            {groups.actionable.map(target => <ConnectionRow key={target.id} target={target} busyAction={busyAction} onApplyTarget={onApplyTarget} onRemoveTarget={onRemoveTarget} onCopyTarget={copyConfiguration} onOpenDirectory={onOpenDirectory} />)}
            {!groups.actionable.length ? <EmptyConnectionRow text="已发现的 AI 工具均已注册" success /> : null}
          </ConnectionSection>

          {groups.manual.length ? <ConnectionSection title="手动注册" description="客户端配置格式需要保留，请在客户端设置中粘贴配置。" count={groups.manual.length} tone="attention">
            {groups.manual.map(target => <ConnectionRow key={target.id} target={target} busyAction={busyAction} onApplyTarget={onApplyTarget} onRemoveTarget={onRemoveTarget} onCopyTarget={copyConfiguration} onOpenDirectory={onOpenDirectory} />)}
          </ConnectionSection> : null}

          {groups.unavailable.length ? <details className="ai-unavailable">
            <summary><div><CircleDashed size={16} /><span><strong>未发现的客户端</strong><small>未安装或暂未被本机识别，不影响其他工具</small></span></div><Pill kind="neutral">{groups.unavailable.length}</Pill></summary>
            <div className="ai-client-list">
              {groups.unavailable.map(target => <ConnectionRow key={target.id} target={target} unavailable busyAction={busyAction} onApplyTarget={onApplyTarget} onRemoveTarget={onRemoveTarget} onCopyTarget={copyConfiguration} onOpenDirectory={onOpenDirectory} />)}
            </div>
          </details> : null}

          {targets.length ? <details className="ai-advanced">
            <summary><Settings2 size={16} /><span><strong>注册诊断</strong><small>查看配置位置、格式和手动配置片段</small></span></summary>
            <div className="ai-diagnostic-list">
              {targets.map(target => {
                const state = targetState(target);
                return <div className="ai-diagnostic-item" key={target.id}>
                  <div className="ai-diagnostic-heading"><strong>{target.name}</strong><Pill kind={state.kind}>{state.label}</Pill></div>
                  <div className="ai-diagnostic-path"><span>配置文件</span><code title={target.config_path}>{target.config_path || '由 Agent 会话管理'}</code></div>
                  <div className="ai-diagnostic-path"><span>配置格式</span><code>{target.config_format || '--'}</code></div>
                  {target.error ? <div className="ai-diagnostic-error">{target.error}</div> : null}
                  {(target.config_preview || target.manual_snippet) ? <details className="ai-config-preview">
                    <summary>查看配置片段</summary>
                    <div className="ai-code-wrap"><pre>{target.config_preview || target.manual_snippet}</pre><button className="btn btn-icon" title="复制配置" aria-label={`复制 ${target.name} 配置`} onClick={() => copyConfiguration(target)}>{copied === target.id ? <Check size={14} /> : <Copy size={14} />}</button></div>
                  </details> : null}
                  {target.config_directory ? <div className="ai-diagnostic-actions"><button className="btn btn-icon" title="打开配置目录" aria-label={`打开 ${target.name} 配置目录`} onClick={() => onOpenDirectory(target.config_directory)}><FolderOpen size={15} /></button></div> : null}
                </div>;
              })}
            </div>
          </details> : null}
        </div>
      )}
    </div>
  );
}

function ConnectionSection({ title, description, count, tone = 'default', children }: { title: string; description: string; count: number; tone?: 'default' | 'attention'; children: ReactNode }) {
  return <section className={`ai-client-section ${tone === 'attention' ? 'has-attention' : ''}`}>
    <div className="ai-section-heading"><div><h3>{title}</h3><span>{description}</span></div><Pill kind={tone === 'attention' ? 'warn' : 'neutral'}>{count}</Pill></div>
    <div className="ai-client-list">{children}</div>
  </section>;
}

function EmptyConnectionRow({ text, success = false }: { text: string; success?: boolean }) {
  return <div className={`ai-empty-row ${success ? 'success' : ''}`}><span className="ai-empty-icon">{success ? <Check size={14} /> : <CircleDashed size={14} />}</span><span>{text}</span></div>;
}

function ConnectionRow({ target, unavailable = false, busyAction, onApplyTarget, onRemoveTarget, onCopyTarget, onOpenDirectory }: { target: McpTargetDescriptor; unavailable?: boolean; busyAction: string | null; onApplyTarget: (targetId: string, resetInvalid?: boolean) => void; onRemoveTarget: (targetId: string) => void; onCopyTarget: (target: McpTargetDescriptor) => void; onOpenDirectory: (path: string) => void }) {
  const builtin = target.id === 'himind-ai';
  const state = targetState(target);
  const pending = busyAction === `target:${target.id}` || busyAction === `remove:${target.id}`;
  return <article className={`ai-client-row${builtin ? ' builtin' : ''}${unavailable ? ' unavailable' : ''}`}>
    <div className={`ai-client-icon ${builtin ? 'himind-ai' : targetIconClass(target.id)}`}><TargetIcon target={target} /></div>
    <div className="ai-client-copy"><strong>{target.name}</strong><span>{targetDescription(target)}</span></div>
    <Pill kind={state.kind}>{unavailable ? '未发现' : state.label}</Pill>
    <div className="ai-client-registration-actions">
      {builtin ? <span className="ai-target-managed"><ShieldCheck size={13} /> 内置</span> : unavailable ? <span className="ai-target-unavailable">未发现</span> : !target.supports_auto_configure ? <><button className="btn btn-icon" title={`复制 ${target.name} MCP 配置`} aria-label={`复制 ${target.name} MCP 配置`} onClick={() => onCopyTarget(target)}><Copy size={15} /></button>{target.config_directory ? <button className="btn btn-icon" title={`打开 ${target.name} 配置目录`} aria-label={`打开 ${target.name} 配置目录`} onClick={() => onOpenDirectory(target.config_directory)}><FolderOpen size={15} /></button> : null}</> : target.state === 'configured' ? <button className="btn btn-icon ai-row-remove" title={`取消 ${target.name} 注册`} aria-label={`取消 ${target.name} 注册`} disabled={pending} onClick={() => onRemoveTarget(target.id)}><Unplug size={15} /></button> : <button className="btn btn-icon btn-primary" title={`${target.state === 'invalid_config' ? '修复' : target.state === 'needs_repair' ? '更新' : '注册 MCP'} ${target.name}`} aria-label={`${target.state === 'invalid_config' ? '修复' : target.state === 'needs_repair' ? '更新' : '注册 MCP'} ${target.name}`} disabled={pending} onClick={() => onApplyTarget(target.id, target.state === 'invalid_config')}><PlugZap size={15} /></button>}
    </div>
  </article>;
}

function TargetIcon({ target }: { target: McpTargetDescriptor }) {
  if (target.id === 'himind-ai') return <PlugZap size={18} />;
  if (target.id.includes('github')) return <Github size={18} />;
  if (target.id === 'codex' || target.id === 'claude-code') return <Code2 size={18} />;
  if (target.id === 'vscode' || target.id === 'cursor') return <MonitorDot size={18} />;
  return <PlugZap size={18} />;
}

function targetIconClass(id: string) {
  if (id.includes('github')) return 'github';
  if (id === 'codex' || id === 'claude-code') return 'code';
  if (id === 'vscode' || id === 'cursor') return 'editor';
  return 'target';
}

function targetState(target: McpTargetDescriptor): { label: string; kind: 'success' | 'warn' | 'danger' | 'neutral' } {
  if (target.id === 'himind-ai') return { label: '已就绪', kind: 'success' };
  if (target.state === 'configured') return { label: '已注册', kind: 'success' };
  if (target.state === 'needs_repair') return { label: '需要更新', kind: 'warn' };
  if (target.state === 'invalid_config') return { label: '配置异常', kind: 'danger' };
  if (!target.detected && target.state === 'not_configured') return { label: '未发现', kind: 'neutral' };
  return { label: target.detected ? '可注册' : '未发现', kind: 'neutral' };
}

function targetDescription(target: McpTargetDescriptor) {
  if (target.id === 'himind-ai') return '会话自动加载本地 MCP、技能和插件能力';
  if (!target.supports_auto_configure && target.state !== 'configured') return '在客户端设置中粘贴 MCP 配置即可注册';
  if (target.state === 'configured') return `MCP 已注册，重启 ${target.name} 后生效`;
  if (!target.detected) return target.config_path ? '客户端未被识别，已有配置不会被修改' : '未在这台电脑上检测到客户端';
  if (target.state === 'invalid_config') return '配置文件格式异常，修复时会先保留备份';
  if (target.state === 'needs_repair') return '当前注册内容与 Agent 配置不一致';
  return '可以注册 HiMind Agent MCP 服务';
}
