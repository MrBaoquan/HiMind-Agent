import { useEffect, useMemo, useState } from 'react';
import { Activity, Check, CircleAlert, CircleCheck, CircleDashed, Code2, Copy, FolderOpen, Github, MonitorDot, PlugZap, Settings2, ShieldCheck, Unplug } from 'lucide-react';
import { Pill } from '../components/Common';
import { BusyIndicator } from '../components/BusyIndicator';
import { useConfirm } from '../components/ConfirmDialog';
import { acpPresets, acpProfileStatus, presetCommand, presetVersion, splitCommandLine, type AcpPreset } from './acpProfileView';
import { buildToolConnections, modelSourceLabel, type ToolConnection } from './aiToolConnections';
import type {
  AcpRuntimeProfileInput,
  AcpRuntimeProfileSnapshot,
  AIServiceListResult,
  CustomAIService,
  DashboardIdentityStatus,
  InferenceGatewayStatus,
  ManagedAIServiceSummary,
  McpConnectionTestResult,
  McpTargetDescriptor,
} from '../services/agentApi';

type AiToolsPanelProps = {
  targets: McpTargetDescriptor[];
  aiServices: AIServiceListResult | null;
  acpProfiles: AcpRuntimeProfileSnapshot | null;
  gatewayStatus: InferenceGatewayStatus | null;
  busyAction: string | null;
  testResult: McpConnectionTestResult | null;
  dashboardEnabled: boolean;
  identity: DashboardIdentityStatus | null;
  onRefresh: () => void;
  onApplyTarget: (targetId: string, resetInvalid?: boolean) => void;
  onApplyAll: () => void;
  onRemoveAll: () => void;
  onRemoveTarget: (targetId: string) => void;
  onOpenDirectory: (path: string) => void;
  onTest: () => void;
  onOpenAccount: () => void;
  onImportAIClient: (target: string, service?: string, replace?: boolean) => Promise<void>;
  onRemoveAIClient: (target: string) => Promise<void>;
  onSetBindingMode: (target: string, mode: 'gateway' | 'direct', service?: string) => Promise<void>;
  gatewayBusy: boolean;
  onRestartGateway: () => void;
  onStopGateway: () => void;
  onSetGatewayPort: (port: number) => Promise<void>;
  onSaveAcpProfile: (input: AcpRuntimeProfileInput) => Promise<void>;
  onSetAcpProfileEnabled: (providerId: string, enabled: boolean) => Promise<void>;
  onRemoveAcpProfile: (providerId: string) => Promise<void>;
};

type Filter = 'all' | 'attention' | 'idle' | 'ready';

export function AiToolsPanel({
  targets,
  aiServices,
  acpProfiles,
  gatewayStatus,
  busyAction,
  testResult,
  dashboardEnabled,
  identity,
  onRefresh,
  onApplyTarget,
  onApplyAll,
  onRemoveAll,
  onRemoveTarget,
  onOpenDirectory,
  onTest,
  onOpenAccount,
  onImportAIClient,
  onRemoveAIClient,
  onSetBindingMode,
  gatewayBusy,
  onRestartGateway,
  onStopGateway,
  onSetGatewayPort,
  onSaveAcpProfile,
  onSetAcpProfileEnabled,
  onRemoveAcpProfile,
}: AiToolsPanelProps) {
  const confirm = useConfirm();
  const [filter, setFilter] = useState<Filter>('all');
  const [expanded, setExpanded] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const [rowBusy, setRowBusy] = useState<string | null>(null);
  const [sourceDraft, setSourceDraft] = useState<Record<string, string>>({});

  const clients = aiServices?.clients?.targets ?? [];
  const customServices = aiServices?.custom?.services ?? [];
  const managed: ManagedAIServiceSummary = aiServices?.managed ?? { available: false };
  const activeServiceId = aiServices?.custom?.active_service_id ?? '';
  const managedLabel = managed.available ? '工作台模型服务' : '工作台模型服务（未就绪）';
  const gatewayClients = gatewayStatus?.gateway_clients ?? [];
  const gatewayClientSet = new Set(gatewayClients.map((entry) => entry.client));

  const connections = useMemo(
    () => buildToolConnections({ targets, clients, acp: acpProfiles }),
    [targets, clients, acpProfiles],
  );

  const detected = connections.filter((item) => item.detected);
  const attention = detected.filter((item) => item.state === 'attention');
  const ready = detected.filter((item) => item.state === 'ready');
  const idle = detected.filter((item) => item.state === 'idle');
  const undetected = connections.filter((item) => !item.detected);
  const actionable = connections.filter((item) => item.mcp && item.mcp.detected && item.mcp.state !== 'configured' && item.mcp.supports_auto_configure);

  const visible = filter === 'attention' ? attention : filter === 'ready' ? ready : filter === 'idle' ? idle : detected;

  const headline = attention.length ? `${attention.length} 个工具待处理` : '工具已就绪';
  const headlineDescription = attention.length
    ? '接入、模型、执行在这里一次管好。'
    : '接入、模型、执行都已接好。';

  const gatewayPortEditable = gatewayClients.length === 0;
  const [portDraft, setPortDraft] = useState('');
  useEffect(() => {
    setPortDraft(gatewayStatus ? String(gatewayStatus.preferred_port) : '');
  }, [gatewayStatus]);
  const parsedPort = Number(portDraft);
  const portValid = portDraft.trim() !== '' && Number.isInteger(parsedPort) && parsedPort >= 1 && parsedPort <= 65535;
  const portChanged = portValid && parsedPort !== gatewayStatus?.preferred_port;

  function toggle(key: string) {
    setExpanded((current) => (current === key ? null : key));
  }

  async function copyConfiguration(target: McpTargetDescriptor) {
    const content = target.config_preview || target.manual_snippet;
    if (!content) return;
    await navigator.clipboard.writeText(content);
    setCopied(target.id);
    window.setTimeout(() => setCopied((current) => (current === target.id ? null : current)), 1800);
  }

  async function runRow(key: string, action: () => Promise<void>) {
    setRowBusy(key);
    try {
      await action();
    } finally {
      setRowBusy(null);
    }
  }

  const defaultSource = activeServiceId ? `custom:${activeServiceId}` : 'managed';

  return (
    <div className="ai-tools-view">
      <section className={`ai-overview ${attention.length ? 'attention' : 'ready'}`}>
        <div className="ai-overview-main">
          <div className="ai-overview-icon">{attention.length ? <CircleAlert size={20} /> : <ShieldCheck size={20} />}</div>
          <div className="ai-overview-copy">
            <span className="ai-overview-eyebrow">工具接入</span>
            <strong>{headline}</strong>
            <span>{headlineDescription}</span>
          </div>
        </div>
        <div className="ai-overview-stats" aria-label="工具接入统计">
          <div><span>已就绪</span><strong>{ready.length}</strong></div>
          <div><span>待处理</span><strong className={attention.length ? 'warning-text' : ''}>{attention.length}</strong></div>
          <div><span>未接入</span><strong>{idle.length}</strong></div>
          <div><span>已发现</span><strong>{detected.length}</strong></div>
        </div>
        <div className="ai-overview-actions">
          <button className="btn btn-primary" disabled={Boolean(busyAction) || !actionable.length} onClick={onApplyAll}>
            <PlugZap size={15} />{busyAction === 'apply-all' ? '接入中' : actionable.length ? '接入全部' : '已接入'}
          </button>
          <button className="btn btn-quiet" disabled={Boolean(busyAction)} onClick={onTest}>{busyAction === 'test' ? <BusyIndicator size={13} /> : <Activity size={15} />}{busyAction === 'test' ? '检查中' : '检查连接'}</button>
        </div>
      </section>

      {testResult ? (
        <div className={`mcp-test-result ${testResult.ok ? 'success' : 'error'}`} role="status">
          <div className="mcp-test-icon">{testResult.ok ? <CircleCheck size={17} /> : <CircleAlert size={17} />}</div>
          <div className="mcp-test-copy">
            <strong>{testResult.ok ? '本机连接正常' : '本机连接异常'}</strong>
            <span>{testResult.ok ? `已发现 ${testResult.capability_count} 项功能` : '请检查应用状态后重试'}</span>
          </div>
        </div>
      ) : null}

      {dashboardEnabled && !identity?.authorized ? <div className="blocker account-blocker"><CircleAlert size={18} /><div><strong>工作台账号未连接</strong><span>不影响本机工具接入；需要工作台数据时再连接账号。</span></div><button className="btn" onClick={onOpenAccount}>连接账号</button></div> : null}

      <section className="ai-client-section">
        <div className="ai-section-heading">
          <div><h3>工具</h3><span>展开任一工具，管理它的接入、模型与执行接线。</span></div>
          {detected.length >= 4 ? (
            <div className="ai-binding-filter" role="group" aria-label="筛选工具">
              <button type="button" aria-pressed={filter === 'all'} onClick={() => setFilter('all')}>全部 <span>{detected.length}</span></button>
              <button type="button" aria-pressed={filter === 'attention'} onClick={() => setFilter('attention')}>待处理 <span>{attention.length}</span></button>
              <button type="button" aria-pressed={filter === 'idle'} onClick={() => setFilter('idle')}>未接入 <span>{idle.length}</span></button>
              <button type="button" aria-pressed={filter === 'ready'} onClick={() => setFilter('ready')}>已就绪 <span>{ready.length}</span></button>
            </div>
          ) : null}
        </div>

        <div className="ai-client-list">
          {visible.map((item) => (
            <ToolRow
              key={item.key}
              item={item}
              expanded={expanded === item.key}
              copied={copied}
              rowBusy={rowBusy === item.key || busyAction === `target:${item.key}` || busyAction === `remove:${item.key}`}
              busyAction={busyAction}
              customServices={customServices}
              managed={managed}
              managedLabel={managedLabel}
              gatewayClientSet={gatewayClientSet}
              defaultSource={defaultSource}
              sourceDraft={sourceDraft[item.key] ?? defaultSource}
              acpProfiles={acpProfiles}
              onToggle={() => toggle(item.key)}
              onApplyTarget={onApplyTarget}
              onRemoveTarget={onRemoveTarget}
              onCopyTarget={copyConfiguration}
              onOpenDirectory={onOpenDirectory}
              onImportAIClient={onImportAIClient}
              onRemoveAIClient={onRemoveAIClient}
              onSetBindingMode={onSetBindingMode}
              onSourceDraft={(value) => setSourceDraft((current) => ({ ...current, [item.key]: value }))}
              onSaveAcpProfile={onSaveAcpProfile}
              onSetAcpProfileEnabled={onSetAcpProfileEnabled}
              onRemoveAcpProfile={onRemoveAcpProfile}
              onRunRow={runRow}
              confirm={confirm}
              gatewayBusy={gatewayBusy}
            />
          ))}
          {!visible.length ? (
            <div className="ai-empty-row"><span className="ai-empty-icon"><CircleCheck size={14} /></span><span>{filter === 'attention' ? '没有待处理的工具' : filter === 'idle' ? '没有未接入的工具' : filter === 'ready' ? '还没有已就绪的工具' : '还没有检测到本机工具'}</span></div>
          ) : null}
        </div>
      </section>

      <details className="ai-advanced ai-gateway-advanced">
        <summary>
          <div className="ai-gateway-advanced-dot"><span className={`status-dot ${gatewayStatus?.running ? 'success' : ''}`} aria-hidden="true" /></div>
          <span>
            <strong>本机推理网关</strong>
            <small>{gatewayStatus?.running ? `${gatewayStatus.url} · 走网关 ${gatewayClients.length}` : gatewayStatus?.last_error || '未启动'}</small>
          </span>
        </summary>
        <div className="ai-gateway-advanced-body">
          <label className="ai-gateway-port" title={gatewayPortEditable ? '端口是写进各 AI 工具配置的地址，改动会重启本机推理网关' : '有工具走网关时不能改端口：请先把这些工具切回直连'}>
            <span>端口</span>
            <input type="number" min={1} max={65535} inputMode="numeric" value={portDraft} disabled={gatewayBusy || !gatewayPortEditable} onChange={(event) => setPortDraft(event.target.value)} />
          </label>
          <button type="button" className="btn ai-service-tool-btn" disabled={gatewayBusy || !gatewayPortEditable || !portChanged} onClick={() => void onSetGatewayPort(parsedPort)}>应用端口</button>
          {gatewayPortEditable ? null : <span className="ai-gateway-port-note">有工具走网关，端口已锁定</span>}
          <button type="button" className="btn ai-service-tool-btn" disabled={gatewayBusy} aria-busy={gatewayBusy} onClick={onRestartGateway}>{gatewayBusy ? <BusyIndicator size={11} /> : null}重启</button>
          <button type="button" className="btn ai-service-tool-btn" disabled={gatewayBusy || !gatewayStatus?.running} onClick={onStopGateway}>停用</button>
        </div>
      </details>

      {undetected.length ? (
        <details className="ai-unavailable">
          <summary>
            <div><CircleDashed size={16} /><span><strong>未检测到的工具</strong><small>未安装或未被识别，不影响其他工具</small></span></div>
            <Pill kind="neutral">{undetected.length}</Pill>
          </summary>
          <ul className="ai-service-supported-list">
            {undetected.map((item) => (
              <li className="undetected" key={item.key}>
                <span>{item.name}</span>
                <span>{item.mcp?.detection_message || '未检测到本机安装'}</span>
              </li>
            ))}
          </ul>
        </details>
      ) : null}

      {targets.length ? (
        <details className="ai-advanced">
          <summary><Settings2 size={16} /><span><strong>连接诊断</strong><small>查看配置位置、格式和手动配置片段</small></span></summary>
          <div className="ai-diagnostic-list">
            {targets.map((target) => {
              const state = mcpState(target);
              return <div className="ai-diagnostic-item" key={target.id}>
                <div className="ai-diagnostic-heading"><strong>{target.name}</strong><Pill kind={state.kind}>{state.label}</Pill></div>
                <div className="ai-diagnostic-path"><span>配置文件</span><code title={target.config_path}>{target.config_path || '由当前会话管理'}</code></div>
                <div className="ai-diagnostic-path"><span>配置格式</span><code>{target.config_format || '--'}</code></div>
                {target.error ? <div className="ai-diagnostic-error">{target.error}</div> : null}
                {(target.config_preview || target.manual_snippet) ? <details className="ai-config-preview">
                  <summary>查看配置片段</summary>
                  <div className="ai-code-wrap"><pre>{target.config_preview || target.manual_snippet}</pre><button className="btn btn-icon" title="复制配置" aria-label={`复制 ${target.name} 配置`} onClick={() => void copyConfiguration(target)}>{copied === target.id ? <Check size={14} /> : <Copy size={14} />}</button></div>
                </details> : null}
                {target.config_directory ? <div className="ai-diagnostic-actions"><button className="btn btn-icon" title="打开配置目录" aria-label={`打开 ${target.name} 配置目录`} onClick={() => onOpenDirectory(target.config_directory)}><FolderOpen size={15} /></button></div> : null}
              </div>;
            })}
          </div>
        </details>
      ) : null}

      {targets.some((target) => target.state === 'configured') ? (
        <details className="ai-advanced ai-danger-advanced">
          <summary><Unplug size={16} /><span><strong>断开全部接入</strong><small>移除所有 AI 工具里的 HiMind MCP 注册</small></span></summary>
          <div className="ai-gateway-advanced-body">
            <button type="button" className="btn btn-danger-quiet ai-service-tool-btn" disabled={Boolean(busyAction)} onClick={() => { void confirm({ title: '断开全部 MCP 接入？', description: '会移除所有 AI 工具里的 HiMind MCP 注册。', confirmText: '全部断开' }).then((accepted) => { if (accepted) onRemoveAll(); }); }}><Unplug size={15} />全部断开</button>
            <span className="ai-danger-note">只移除 HiMind 写入的注册，各工具自己的配置保留。</span>
          </div>
        </details>
      ) : null}
    </div>
  );
}

function mcpState(target: McpTargetDescriptor): { label: string; kind: 'success' | 'warn' | 'danger' | 'neutral' } {
  if (target.id === 'himind-ai') return { label: '已就绪', kind: 'success' };
  if (target.state === 'configured') return { label: '已接入', kind: 'success' };
  if (target.state === 'needs_repair') return { label: '需更新', kind: 'warn' };
  if (target.state === 'invalid_config') return { label: '配置异常', kind: 'danger' };
  if (!target.detected) return { label: '未安装', kind: 'neutral' };
  return { label: target.supports_auto_configure ? '可接入' : '需手动', kind: 'neutral' };
}

/**
 * 主按钮用动词，不要拿状态当按钮文案：状态（可接入 / 需更新 / 配置异常）是「现在怎样」，
 * 按钮要说「点下去会做什么」。两者分开，用户才不会对着一个写着「配置异常」的按钮发懵。
 */
function mcpActionLabel(target: McpTargetDescriptor): string {
  if (target.state === 'invalid_config') return '修复';
  if (target.state === 'needs_repair') return '更新';
  return '接入';
}

function toolSummary(item: ToolConnection, services: CustomAIService[], managed: ManagedAIServiceSummary, gatewayClientSet: Set<string>): string {
  if (item.builtin) return '会话自动加载本机插件和技能';
  const parts: string[] = [];
  if (item.mcp) parts.push(`接入 ${mcpState(item.mcp).label}`);
  if (item.model && item.model.state === 'imported') {
    const mode = gatewayClientSet.has(item.key) ? '经网关' : '直连';
    parts.push(`模型 ${mode} ${modelSourceLabel(item.model.service, services, managed)}`);
  } else if (item.model && item.model.client_detected) {
    parts.push('模型 未分发');
  }
  if (item.exec) parts.push(item.exec.enabled ? '执行 已接入' : '执行 已停用');
  else if (item.capabilities.exec) parts.push('执行 未接入');
  return parts.join(' · ') || '未检测到';
}

function ToolIcon({ item }: { item: ToolConnection }) {
  if (item.builtin) return <PlugZap size={18} />;
  if (item.key.includes('github')) return <Github size={18} />;
  if (item.key === 'codex' || item.key === 'claude-code' || item.key === 'opencode') return <Code2 size={18} />;
  if (item.key === 'vscode' || item.key === 'cursor' || item.key.startsWith('vscode')) return <MonitorDot size={18} />;
  return <PlugZap size={18} />;
}

function toolIconClass(item: ToolConnection) {
  if (item.builtin) return 'himind-ai';
  if (item.key.includes('github')) return 'github';
  if (item.key === 'codex' || item.key === 'claude-code' || item.key === 'opencode') return 'code';
  if (item.key === 'vscode' || item.key === 'cursor' || item.key.startsWith('vscode')) return 'editor';
  return 'target';
}

function ToolRow(props: {
  item: ToolConnection;
  expanded: boolean;
  copied: string | null;
  rowBusy: boolean;
  busyAction: string | null;
  customServices: CustomAIService[];
  managed: ManagedAIServiceSummary;
  managedLabel: string;
  gatewayClientSet: Set<string>;
  defaultSource: string;
  sourceDraft: string;
  acpProfiles: AcpRuntimeProfileSnapshot | null;
  onToggle: () => void;
  onApplyTarget: (targetId: string, resetInvalid?: boolean) => void;
  onRemoveTarget: (targetId: string) => void;
  onCopyTarget: (target: McpTargetDescriptor) => void;
  onOpenDirectory: (path: string) => void;
  onImportAIClient: (target: string, service?: string, replace?: boolean) => Promise<void>;
  onRemoveAIClient: (target: string) => Promise<void>;
  onSetBindingMode: (target: string, mode: 'gateway' | 'direct', service?: string) => Promise<void>;
  onSourceDraft: (value: string) => void;
  onSaveAcpProfile: (input: AcpRuntimeProfileInput) => Promise<void>;
  onSetAcpProfileEnabled: (providerId: string, enabled: boolean) => Promise<void>;
  onRemoveAcpProfile: (providerId: string) => Promise<void>;
  onRunRow: (key: string, action: () => Promise<void>) => Promise<void>;
  confirm: ReturnType<typeof useConfirm>;
  gatewayBusy: boolean;
}) {
  const {
    item, expanded, rowBusy, busyAction, customServices, managed, managedLabel, gatewayClientSet,
    defaultSource, sourceDraft, acpProfiles, onToggle, onApplyTarget, onRemoveTarget, onCopyTarget,
    onOpenDirectory, onImportAIClient, onRemoveAIClient, onSetBindingMode, onSourceDraft,
    onSaveAcpProfile, onSetAcpProfileEnabled, onRemoveAcpProfile, onRunRow, confirm,
  } = props;

  const mcpPending = busyAction === `target:${item.key}` || busyAction === `remove:${item.key}`;
  const modelState = item.model?.state ?? '';
  const modelImported = modelState === 'imported';
  const gatewayMode = gatewayClientSet.has(item.key);
  const summary = toolSummary(item, customServices, managed, gatewayClientSet);
  const rowState = item.state;

  const sourceOptions: Array<{ value: string; label: string; disabled?: boolean }> = [
    { value: 'managed', label: managedLabel, disabled: !managed.available },
    ...customServices.map((service) => ({ value: `custom:${service.id}`, label: service.display_name })),
  ];

  async function changeSource(next: string) {
    if (!next || next === (item.model?.service ?? '')) return;
    if (gatewayMode) {
      await onRunRow(item.key, () => onSetBindingMode(item.key, 'gateway', next));
    } else {
      await onRunRow(item.key, () => onImportAIClient(item.key, next, true));
    }
  }

  return (
    <article className={`ai-client-row ai-tool-row${item.builtin ? ' builtin' : ''}${rowState === 'attention' ? ' has-attention' : ''}${expanded ? ' expanded' : ''}`}>
      <div className={`ai-client-icon ${toolIconClass(item)}`}><ToolIcon item={item} /></div>
      <div className="ai-client-copy">
        <span className="ai-tool-head">
          <strong>{item.name}</strong>
          <Pill kind={item.builtin ? 'neutral' : rowState === 'attention' ? 'warn' : rowState === 'ready' ? 'success' : 'neutral'}>
            {item.builtin ? '内置' : rowState === 'attention' ? '待处理' : rowState === 'ready' ? '已就绪' : '未接入'}
          </Pill>
        </span>
        <span title={summary}>{summary}</span>
      </div>
      <div className="ai-client-registration-actions">
        {item.builtin ? <span className="ai-target-managed"><ShieldCheck size={13} /> 无需配置</span>
          : <button type="button" className="btn ai-service-tool-btn" aria-expanded={expanded} onClick={onToggle}>{expanded ? '收起' : '管理'}</button>}
      </div>

      {expanded && !item.builtin ? (
        <div className="ai-service-detail ai-tool-detail">
          {item.mcp ? (
            <div className="ai-tool-wire">
              <span className="ai-tool-wire-kind">接入</span>
              <span className="ai-tool-wire-state">
                <span className={`status-dot ${item.mcp.state === 'configured' ? 'success' : item.mcp.state === 'needs_repair' || item.mcp.state === 'invalid_config' ? 'warn' : ''}`} />
                {mcpState(item.mcp).label}
                {item.mcp.supports_auto_configure ? null : <span className="muted">· 需手动粘贴配置</span>}
                {item.mcp.state === 'configured' && item.mcp.restart_required ? <span className="muted">· 重启客户端后生效</span> : null}
              </span>
              <span className="ai-tool-wire-actions">
                {!item.mcp.supports_auto_configure && item.mcp.state !== 'configured' ? <>
                  <button type="button" className="btn ai-service-tool-btn" onClick={() => void onCopyTarget(item.mcp!)}><Copy size={14} />复制配置</button>
                  {item.mcp.config_directory ? <button type="button" className="btn ai-service-tool-btn" onClick={() => onOpenDirectory(item.mcp!.config_directory)}><FolderOpen size={14} />打开目录</button> : null}
                </> : item.mcp.state === 'configured' ? (
                  <button type="button" className="btn ai-service-tool-btn" disabled={mcpPending} onClick={() => onRemoveTarget(item.mcp!.id)}><Unplug size={14} />断开</button>
                ) : (
                  <button type="button" className="btn btn-primary ai-service-tool-btn" disabled={mcpPending} onClick={() => onApplyTarget(item.mcp!.id, item.mcp!.state === 'invalid_config')}><PlugZap size={14} />{mcpPending ? '处理中' : mcpActionLabel(item.mcp!)}</button>
                )}
              </span>
            </div>
          ) : null}

          {item.capabilities.model ? (
            <div className="ai-tool-wire">
              <span className="ai-tool-wire-kind">模型</span>
              <span className="ai-tool-wire-state">
                {modelImported ? <>
                  <span className={`status-dot ${gatewayMode ? 'warn' : 'success'}`} />
                  <select className="ai-service-route-select ai-tool-source-select" aria-label={`${item.name} 的模型来源`} title={sourceOptions.some((option) => option.value === (item.model?.service ?? '')) ? undefined : '当前绑定的来源没有记录，可在这里重新选择'} value={item.model?.service ?? ''} disabled={rowBusy} onChange={(event) => void changeSource(event.target.value)}>
                    {sourceOptions.map((option) => <option key={option.value} value={option.value} disabled={option.disabled}>{option.label}</option>)}
                    {sourceOptions.some((option) => option.value === (item.model?.service ?? '')) ? null : <option value={item.model?.service ?? ''}>未知来源</option>}
                  </select>
                  <span className="muted">· {gatewayMode ? '经本机网关' : '直连'}</span>
                </> : <>
                  <span className="status-dot" />
                  <span className="muted">未分发</span>
                  <select className="ai-service-route-select ai-tool-source-select" aria-label={`${item.name} 的模型来源`} value={sourceDraft} disabled={rowBusy} onChange={(event) => onSourceDraft(event.target.value)}>
                    {sourceOptions.map((option) => <option key={option.value} value={option.value} disabled={option.disabled}>{option.label}</option>)}
                  </select>
                </>}
              </span>
              <span className="ai-tool-wire-actions">
                {modelImported ? <>
                  <button type="button" className="btn ai-service-tool-btn" disabled={rowBusy} title={gatewayMode ? '切回直连：把真实地址与密钥写回该工具' : '切到网关：密钥留在 Agent'} onClick={() => void onRunRow(item.key, () => onSetBindingMode(item.key, gatewayMode ? 'direct' : 'gateway'))}>{rowBusy ? <BusyIndicator size={11} /> : null}{gatewayMode ? '切回直连' : '切到网关'}</button>
                  <button type="button" className="btn ai-service-tool-btn" disabled={rowBusy} onClick={() => { void confirm({ title: `取消分发到 ${item.name}？`, description: '取消后该工具恢复自己的模型配置，服务仍可再分发。', confirmText: '取消分发' }).then((accepted) => { if (accepted) void onRunRow(item.key, () => onRemoveAIClient(item.key)); }); }}>{rowBusy ? <BusyIndicator size={11} /> : null}取消分发</button>
                </> : (
                  <button type="button" className="btn btn-primary ai-service-tool-btn" disabled={rowBusy || !sourceDraft || (sourceDraft === 'managed' && !managed.available)} onClick={() => void onRunRow(item.key, () => onImportAIClient(item.key, sourceDraft))}>{rowBusy ? <BusyIndicator size={11} /> : null}分发</button>
                )}
              </span>
            </div>
          ) : null}

          {item.capabilities.exec ? <ExecWire item={item} acpProfiles={acpProfiles} rowBusy={rowBusy} onRunRow={onRunRow} onSaveAcpProfile={onSaveAcpProfile} onSetAcpProfileEnabled={onSetAcpProfileEnabled} onRemoveAcpProfile={onRemoveAcpProfile} confirm={confirm} /> : null}
        </div>
      ) : null}
    </article>
  );
}

function ExecWire({ item, acpProfiles, rowBusy, onRunRow, onSaveAcpProfile, onSetAcpProfileEnabled, onRemoveAcpProfile, confirm }: {
  item: ToolConnection;
  acpProfiles: AcpRuntimeProfileSnapshot | null;
  rowBusy: boolean;
  onRunRow: (key: string, action: () => Promise<void>) => Promise<void>;
  onSaveAcpProfile: (input: AcpRuntimeProfileInput) => Promise<void>;
  onSetAcpProfileEnabled: (providerId: string, enabled: boolean) => Promise<void>;
  onRemoveAcpProfile: (providerId: string) => Promise<void>;
  confirm: ReturnType<typeof useConfirm>;
}) {
  const preset: AcpPreset | undefined = acpPresets.find((candidate) => candidate.providerId === item.key);
  const providers = acpProfiles?.providers ?? [];
  const executables = acpProfiles?.executables ?? {};

  async function connect() {
    if (!preset) return;
    const command = presetCommand(preset, executables);
    const tokens = splitCommandLine(command) ?? [];
    if (!tokens.length) return;
    await onRunRow(item.key, () => onSaveAcpProfile({
      providerId: preset.providerId,
      displayName: preset.name,
      executable: tokens[0],
      args: tokens.slice(1),
      version: presetVersion(preset, executables),
      permissionPolicy: preset.permissionPolicy,
      enabled: true,
    }));
  }

  if (item.exec) {
    const status = acpProfileStatus(item.exec, providers.find((candidate) => candidate.provider === item.exec!.provider_id));
    return (
      <div className="ai-tool-wire">
        <span className="ai-tool-wire-kind">执行</span>
        <span className="ai-tool-wire-state"><Pill kind={status.tone}>{status.label}</Pill>{status.reason ? <span className="muted">{status.reason}</span> : <span className="muted">· 工作流 AI 步骤可选用</span>}</span>
        <span className="ai-tool-wire-actions">
          <button type="button" className="btn ai-service-tool-btn" disabled={rowBusy} onClick={() => void onRunRow(item.key, () => onSetAcpProfileEnabled(item.exec!.provider_id, !item.exec!.enabled))}>{rowBusy ? <BusyIndicator size={11} /> : null}{item.exec.enabled ? '停用' : '启用'}</button>
          <button type="button" className="btn ai-service-tool-btn" disabled={rowBusy} onClick={() => { void confirm({ title: `删除运行环境「${item.name}」？`, description: '移除后，工作流不能再选它执行。', confirmText: '删除' }).then((accepted) => { if (accepted) void onRunRow(item.key, () => onRemoveAcpProfile(item.exec!.provider_id)); }); }}>删除</button>
        </span>
      </div>
    );
  }

  if (!preset) return null;
  return (
    <div className="ai-tool-wire">
      <span className="ai-tool-wire-kind">执行</span>
      <span className="ai-tool-wire-state"><span className="status-dot" /><span className="muted">未接入</span><span className="muted">· 工作流的 AI 步骤可选它执行</span></span>
      <span className="ai-tool-wire-actions">
        <button type="button" className="btn btn-primary ai-service-tool-btn" disabled={rowBusy} onClick={() => void connect()}>{rowBusy ? <BusyIndicator size={11} /> : null}接入</button>
      </span>
    </div>
  );
}
