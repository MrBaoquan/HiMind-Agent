import { useState } from 'react';
import { AI_SERVICE_CAPABILITIES, AI_SERVICE_CAPABILITY_LABELS as labels, type AIServiceCapability as Capability } from './aiServiceCapabilities';
import { configuredOperations, modelSourceLabels, type ModelSetup } from './aiServiceOnboarding';

export function AiModelCatalogEditor({ value, onChange, disabled = false }: {
  value: ModelSetup; onChange: (value: ModelSetup) => void; disabled?: boolean;
}) {
  const [query, setQuery] = useState('');
  const [manualId, setManualId] = useState('');
  const [manualOperation, setManualOperation] = useState<Capability>('chat');
  const [manualError, setManualError] = useState('');
  function changeOperation(id: string, operation: Capability) {
    onChange({ ...value, entries: value.entries.map(entry => entry.id !== id ? entry : {
      ...entry, operations: entry.operations.includes(operation)
        ? entry.operations.filter(op => op !== operation) : [...entry.operations, operation],
    }) });
  }
  function addModel() {
    const id = manualId.trim();
    if (!id || id.length > 160) { setManualError('请输入不超过 160 字符的模型 ID。'); return; }
    const exists = value.entries.some(entry => entry.id === id);
    onChange({ ...value, entries: exists ? value.entries.map(entry => entry.id !== id ? entry : {
      ...entry, selected: true, operations: [...new Set([...entry.operations, manualOperation])],
    }) : [...value.entries, { id, selected: true, operations: [manualOperation], suggestions: [], source: 'manual' }] });
    setManualId(''); setManualError(''); setQuery('');
  }
  const filtered = value.entries.filter(entry => entry.id.toLowerCase().includes(query.trim().toLowerCase()));
  return <div id="ai-service-capabilities" tabIndex={-1} className="ai-model-editor">
    {value.entries.length > 5 ? <label className="field-label"><span>搜索模型</span><input value={query} onChange={event => setQuery(event.target.value)} placeholder="按模型 ID 搜索" /></label> : null}
    <div className="ai-model-list">
      {filtered.map(entry => <article key={entry.id} className={`ai-model-row${entry.selected ? ' selected' : ''}`}>
        <label className="ai-model-title"><input type="checkbox" aria-label={`使用模型 ${entry.id}`} checked={entry.selected} disabled={disabled} onChange={event => onChange({ ...value, entries: value.entries.map(item => item.id === entry.id ? { ...item, selected: event.target.checked } : item) })} /><strong>{entry.id}</strong></label>
        <small>{modelSourceLabels[entry.source]} · 未做实际调用验证</small>
        {entry.selected ? <div className="ai-model-uses">
          <span>用途</span>
          {entry.operations.map(op => <button key={op} type="button" className="btn ai-model-use" disabled={disabled} title={`移除 ${labels[op]}`} onClick={() => changeOperation(entry.id, op)}>{labels[op]} ×</button>)}
          <select aria-label={`${entry.id} 添加用途`} value="" disabled={disabled} onChange={event => changeOperation(entry.id, event.target.value as Capability)}>
            <option value="">{entry.operations.length ? '添加用途…' : '选择用途…'}</option>
            {AI_SERVICE_CAPABILITIES.filter(op => !entry.operations.includes(op)).map(op => <option key={op} value={op}>{labels[op]}</option>)}
          </select>
          {!entry.operations.length ? <small>能力信息未知，请按供应商说明选择。</small> : null}
        </div> : entry.suggestions.length ? <small>预设建议：{entry.suggestions.map(op => labels[op]).join('、')}</small> : <small>能力信息未知</small>}
      </article>)}
      {!filtered.length ? <p className="ai-service-hint">{value.entries.length ? '没有匹配的模型。' : '尚无模型目录，可从服务获取，也可手动添加。'}</p> : null}
    </div>
    <details className="ai-onboarding-details"><summary>手动添加模型</summary>
      <div className="ai-manual-model">
        <label className="field-label"><span>模型 ID</span><input value={manualId} disabled={disabled} onChange={event => setManualId(event.target.value)} placeholder="供应商提供的完整模型 ID" /></label>
        <label className="field-label"><span>模型用途</span><select aria-label="模型用途" value={manualOperation} disabled={disabled} onChange={event => setManualOperation(event.target.value as Capability)}>{AI_SERVICE_CAPABILITIES.map(op => <option value={op} key={op}>{labels[op]}</option>)}</select></label>
        <button type="button" className="btn" disabled={disabled} onClick={addModel}>添加模型</button>
      </div>
      {manualError ? <p role="alert">{manualError}</p> : null}
    </details>
    {configuredOperations(value).length ? <details className="ai-onboarding-details"><summary>各用途的服务默认模型</summary>
      <p className="ai-service-hint">只有同一用途有多个模型时才需要调整；不会改变 Agent 全局默认路由。</p>
      {configuredOperations(value).map(op => {
        const ids = value.entries.filter(entry => entry.selected && entry.operations.includes(op)).map(entry => entry.id);
        return <label className="field-label" key={op}><span>{labels[op]}默认模型</span><select aria-label={`${labels[op]}默认模型`} disabled={disabled} value={ids.includes(value.defaults[op] ?? '') ? value.defaults[op] : ids[0]} onChange={event => onChange({ ...value, defaults: { ...value.defaults, [op]: event.target.value } })}>{ids.map(id => <option key={id}>{id}</option>)}</select></label>;
      })}
    </details> : null}
  </div>;
}
