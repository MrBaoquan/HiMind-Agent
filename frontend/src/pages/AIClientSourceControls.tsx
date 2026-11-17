import { useRef, useState } from 'react';
import { useConfirm } from '../components/ConfirmDialog';
import { BusyIndicator } from '../components/BusyIndicator';
import { agentApi, type AIClientSourceRequest, type AIProviderImportStatus } from '../services/agentApi';

export function configuredSourceLabel(status: AIProviderImportStatus): string {
  const source = status.source;
  if (!source) return status.state === 'imported' ? 'Agent 模型来源已登记' : '使用客户端自身配置';
  switch (source.configured_source) {
    case 'agent': return '默认来源：Agent 服务';
    case 'native_provider': return '默认来源：Codex 原生供应商';
    case 'other_provider': return '默认来源：其他供应商';
    case 'unknown': return '当前配置来源无法确认';
    default: return '模型由客户端选择';
  }
}

/** Host-declared capabilities drive the controls; no client-specific config writes in the UI. */
export function AIClientSourceControls({ status, disabled, onChanged, onRun }: {
  status: AIProviderImportStatus;
  disabled: boolean;
  onChanged: () => void;
  onRun: (key: string, action: () => Promise<void>) => Promise<void>;
}) {
  const confirm = useConfirm();
  const [model, setModel] = useState('');
  const [notice, setNotice] = useState('');
  const [failed, setFailed] = useState(false);
  const [busy, setBusy] = useState(false);
  const pending = useRef(false);
  const source = status.source;
  if (!source) return null;
  const caps = source.capabilities;
  const models = status.models ?? [];
  const selected = model || (models[0] ?? '');
  const selectedAvailable = models.includes(selected);

  async function apply(action: AIClientSourceRequest['action']) {
    if (pending.current || disabled) return;
    pending.current = true; setBusy(true); setNotice(''); setFailed(false);
    try {
      await onRun(status.target, async () => {
        const input: AIClientSourceRequest = { target: status.target, action, ...(action === 'set_default' ? { model: selected } : {}) };
        const plan = await agentApi.planAIClientSource(input);
        const accepted = await confirm({
          title: action === 'native_account' ? '改用客户端原生账号？' : action === 'restore_previous' ? '恢复分发前配置？' : '设置客户端默认模型？',
          description: [plan.detail, `配置：${plan.config_path}`, ...plan.changes, ...plan.warnings, plan.load_hint].join('\n'),
          confirmText: plan.conflict ? '确认变更并覆盖' : '应用配置', danger: false,
        });
        if (!accepted) return;
        const result = await agentApi.applyAIClientSource({ ...input, expected_revision: plan.revision, accept_drift: plan.conflict });
        setNotice(`配置已写入，客户端生效待核对。${result.load_hint}`);
        onChanged();
      });
    } catch (error) { setFailed(true); setNotice(String(error)); }
    finally { pending.current = false; setBusy(false); }
  }

  return <div className="ai-client-source" aria-label={`${status.target} 来源与账号`}>
    <div className="ai-client-source-summary"><strong>{configuredSourceLabel(status)}</strong><span className="muted">账号与实际会话状态：请在客户端核对</span></div>
    <p className="muted">{caps.selection_hint}</p>
    <div className="ai-client-source-actions">
      {caps.native_account ? <button className="btn ai-service-tool-btn" type="button" disabled={disabled || busy} onClick={() => void apply('native_account')}>{busy ? <BusyIndicator size={11} /> : null}{status.target === 'codex' ? '使用 ChatGPT 账号' : '使用原生账号'}</button> : null}
      {source.can_restore ? <button className="btn ai-service-tool-btn" type="button" disabled={disabled || busy} onClick={() => void apply('restore_previous')}>恢复分发前配置</button> : null}
      {caps.select_default && models.length ? <>
        <select className="ai-service-route-select" aria-label={`${status.target} 默认模型`} value={selected} disabled={disabled || busy} onChange={(event) => setModel(event.target.value)}>{!selectedAvailable && selected ? <option value={selected}>{selected}（已不可用，请重新选择）</option> : null}{models.map((id) => <option key={id} value={id}>{id}</option>)}</select>
        <button className="btn ai-service-tool-btn" type="button" disabled={disabled || busy || !selectedAvailable} onClick={() => void apply('set_default')}>设为默认模型</button>
      </> : null}
    </div>
    <details><summary>配置范围与生效说明</summary><p>{source.config_path || '由客户端管理'}</p><p>{caps.load_hint}</p>{source.default_model ? <p>配置中的默认模型：{source.default_model}</p> : null}{source.warnings.map((warning) => <p key={warning}>{warning}</p>)}</details>
    {notice ? <p role={failed ? 'alert' : 'status'} className={failed ? 'warning-text' : 'muted'}>{notice}</p> : null}
  </div>;
}
