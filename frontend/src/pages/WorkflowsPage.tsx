import { useEffect, useMemo, useState } from 'react';
import { CheckCircle2, Clock3, FileText, Play, RefreshCw, ShieldCheck, Workflow, X } from 'lucide-react';
import { EmptyState, PageHeader, Pill } from '../components/Common';
import type { WorkflowCenterSnapshot, WorkflowLocalRun, WorkflowRunSnapshot, WorkflowViewField } from '../services/agentApi';

type WorkflowsPageProps = {
  snapshot: WorkflowCenterSnapshot | null;
  loading: boolean;
  error: string;
  onRefresh: () => void;
  onLoadRun: (runId: string) => Promise<WorkflowRunSnapshot>;
  onApprove: (runId: string, stepId: string) => Promise<void>;
  onReject: (runId: string, stepId: string) => Promise<void>;
  onResume: (runId: string) => Promise<void>;
  onCancel: (runId: string) => Promise<void>;
  onStart: (packageId: string, input: Record<string, unknown>) => Promise<void>;
};

function statusKind(status: string): 'success' | 'warn' | 'danger' | 'neutral' {
  if (status === 'succeeded') return 'success';
  if (status === 'failed' || status === 'canceled') return 'danger';
  if (status === 'waiting' || status === 'running') return 'warn';
  return 'neutral';
}

function statusLabel(status: string) {
  return ({
    queued: '排队中',
    running: '执行中',
    waiting: '等待操作',
    succeeded: '已完成',
    failed: '失败',
    canceled: '已取消',
    pending: '待执行',
    skipped: '已跳过',
  } as Record<string, string>)[status] || status;
}

function formatTime(value: string) {
  if (!value) return '--';
  const numeric = Number(value);
  const date = Number.isFinite(numeric) ? new Date(numeric * 1000) : new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

type WorkflowStartField = {
  id: string;
  label: string;
  type: string;
  required: boolean;
  defaultValue: unknown;
  options: string[];
  placeholder: string;
  target: string;
};

function normalizeStartField(field: WorkflowViewField): WorkflowStartField {
  if (typeof field === 'string') {
    const type = field === 'credential_handles'
      ? 'credential'
      : field === 'acceptance_criteria' || field === 'constraints' || field === 'scripts' || field === 'evidence_paths'
        ? 'list'
        : field === 'passed' || field === 'rollback_requested'
          ? 'boolean'
          : field === 'package_manager' || field === 'install_mode' || field === 'environment'
            ? 'select'
            : 'text';
    const options = field === 'package_manager'
      ? ['npm', 'pnpm', 'yarn']
      : field === 'install_mode'
        ? ['install', 'ci']
        : field === 'environment'
          ? ['development', 'staging', 'production']
          : [];
    return {
      id: field,
      label: field,
      type,
      required: field === 'requirement' || field === 'acceptance_criteria' || field === 'workspace_root' || field === 'project_root' || field === 'app_id',
      defaultValue: field === 'passed' ? true : field === 'rollback_requested' ? false : field === 'package_manager' ? 'npm' : field === 'install_mode' ? 'install' : field === 'environment' ? 'development' : '',
      options,
      placeholder: '',
      target: field === 'credential_handles' ? 'private_key_path' : '',
    };
  }
  return {
    id: field.id,
    label: field.label || field.id,
    type: field.type || 'text',
    required: Boolean(field.required),
    defaultValue: field.default,
    options: field.options || [],
    placeholder: field.placeholder || '',
    target: field.target || '',
  };
}

function initialFieldValue(field: WorkflowStartField): unknown {
  if (field.defaultValue !== undefined) {
    if (field.type === 'list' && Array.isArray(field.defaultValue)) return field.defaultValue.join('\n');
    return field.defaultValue;
  }
  if (field.type === 'boolean') return false;
  if (field.type === 'list') return '';
  if (field.type === 'credential') return field.target === 'private_key_path' ? 'wechat-upload-private-key' : '';
  if (field.type === 'json') return '{}';
  return '';
}

function fieldsToInput(fields: WorkflowStartField[], values: Record<string, unknown>): Record<string, unknown> {
  const input: Record<string, unknown> = {};
  for (const field of fields) {
    const value = values[field.id];
    if (field.type === 'credential') {
      const handle = String(value || '').trim();
      if (handle) input[field.id] = { [field.target || 'value']: handle };
      continue;
    }
    if (field.type === 'list') {
      input[field.id] = String(value || '').split(/\r?\n/).map(item => item.trim()).filter(Boolean);
      continue;
    }
    if (field.type === 'number') {
      input[field.id] = value === '' ? 0 : Number(value);
      continue;
    }
    if (field.type === 'json') {
      input[field.id] = JSON.parse(String(value || '{}'));
      continue;
    }
    input[field.id] = value ?? '';
  }
  return input;
}

function jsonToFormValues(fields: WorkflowStartField[], input: Record<string, unknown>): Record<string, unknown> {
  const values: Record<string, unknown> = {};
  for (const field of fields) {
    const value = input[field.id];
    if (field.type === 'credential') {
      values[field.id] = value && typeof value === 'object' && !Array.isArray(value)
        ? String((value as Record<string, unknown>)[field.target || 'value'] || '')
        : '';
    } else if (field.type === 'list') {
      values[field.id] = Array.isArray(value) ? value.join('\n') : String(value || '');
    } else if (field.type === 'json') {
      values[field.id] = JSON.stringify(value ?? {}, null, 2);
    } else {
      values[field.id] = value ?? initialFieldValue(field);
    }
  }
  return values;
}

export function WorkflowsPage({ snapshot, loading, error, onRefresh, onLoadRun, onApprove, onReject, onResume, onCancel, onStart }: WorkflowsPageProps) {
  const [selectedWorkflowId, setSelectedWorkflowId] = useState('');
  const [selectedRunId, setSelectedRunId] = useState('');
  const [runDetail, setRunDetail] = useState<WorkflowRunSnapshot | null>(null);
  const [runLoading, setRunLoading] = useState(false);
  const [runError, setRunError] = useState('');
  const [actionBusy, setActionBusy] = useState('');
  const [startOpen, setStartOpen] = useState(false);
  const [startInput, setStartInput] = useState('');
  const [startError, setStartError] = useState('');
  const [startBusy, setStartBusy] = useState(false);
  const [startMode, setStartMode] = useState<'form' | 'json'>('form');
  const [startForm, setStartForm] = useState<Record<string, unknown>>({});

  const workflows = snapshot?.workflows || [];
  const runs = snapshot?.runs || [];
  const selectedWorkflow = useMemo(
    () => workflows.find(item => item.package.id === selectedWorkflowId) || workflows[0] || null,
    [selectedWorkflowId, workflows],
  );

  useEffect(() => {
    if (!selectedWorkflowId && workflows[0]) setSelectedWorkflowId(workflows[0].package.id);
  }, [selectedWorkflowId, workflows]);

  async function openRun(runId: string) {
    setSelectedRunId(runId);
    setRunLoading(true);
    setRunError('');
    try {
      setRunDetail(await onLoadRun(runId));
    } catch {
      setRunDetail(null);
      setRunError('运行详情读取失败');
    } finally {
      setRunLoading(false);
    }
  }

  async function performRunAction(runId: string, action: string, operation: () => Promise<void>) {
    setActionBusy(action);
    setRunError('');
    try {
      await operation();
      await openRun(runId);
    } catch {
      setRunError('工作流操作失败，请查看 Agent 日志后重试');
    } finally {
      setActionBusy('');
    }
  }

  const waiting = runs.filter(item => item.run.status === 'waiting').length;
  const succeeded = runs.filter(item => item.run.status === 'succeeded').length;
  const artifacts = selectedWorkflow?.package.artifacts.length || 0;
  const waitingForFeedback = Boolean(runDetail && runDetail.run.status === 'waiting' && runDetail.events.some(event =>
    event.step_id === runDetail.run.current_step_id
    && typeof event.payload === 'object'
    && event.payload !== null
    && (event.payload as { waiting_for_feedback?: boolean }).waiting_for_feedback === true,
  ));

  function openStart() {
    if (!selectedWorkflow) return;
    const values: Record<string, unknown> = {};
    for (const field of normalizedStartFields(selectedWorkflow)) values[field.id] = initialFieldValue(field);
    setStartForm(values);
    setStartInput(JSON.stringify(fieldsToInput(normalizedStartFields(selectedWorkflow), values), null, 2));
    setStartMode('form');
    setStartError('');
    setStartOpen(true);
  }

  function normalizedStartFields(workflow: NonNullable<typeof selectedWorkflow>) {
    return (workflow.view?.sections || []).flatMap(section => (section.fields || []).map(normalizeStartField));
  }

  function switchStartMode(mode: 'form' | 'json') {
    if (mode === startMode) return;
    const fields = normalizedStartFields(selectedWorkflow!);
    if (mode === 'json') {
      try {
        setStartInput(JSON.stringify(fieldsToInput(fields, startForm), null, 2));
        setStartError('');
        setStartMode(mode);
      } catch {
        setStartError('结构化字段暂时无法转换为 JSON，请检查列表或 JSON 字段');
      }
      return;
    }
    try {
      const parsed = JSON.parse(startInput);
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('not object');
      setStartForm(jsonToFormValues(fields, parsed));
      setStartError('');
      setStartMode(mode);
    } catch {
      setStartError('JSON 需要是有效对象，才能切换回表单');
    }
  }

  async function submitStart() {
    if (!selectedWorkflow) return;
    setStartBusy(true);
    setStartError('');
    try {
      let input: Record<string, unknown>;
      if (startMode === 'form') {
        const fields = normalizedStartFields(selectedWorkflow);
        const missing = fields.find(field => field.required && !String(startForm[field.id] ?? '').trim());
        if (missing) throw new Error(`missing:${missing.label}`);
        input = fieldsToInput(fields, startForm);
      } else {
        const parsed = JSON.parse(startInput);
        if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
          throw new Error('input must be an object');
        }
        input = parsed;
      }
      await onStart(selectedWorkflow.package.id, input);
      setStartOpen(false);
    } catch (submitError) {
      const message = submitError instanceof Error ? submitError.message : '';
      setStartError(message.startsWith('missing:')
        ? `请填写：${message.slice('missing:'.length)}`
        : startMode === 'json'
          ? '输入必须是有效的 JSON 对象'
          : '结构化字段需要是有效内容；列表和 JSON 字段请检查格式');
    } finally {
      setStartBusy(false);
    }
  }

  function setStartField(fieldId: string, value: unknown) {
    setStartForm(current => ({ ...current, [fieldId]: value }));
  }

  return (
    <div className="workflow-page">
      <PageHeader
        title="工作流"
        description="本地业务包、执行事实和交付产物"
        actions={<button className="btn btn-icon" title="刷新工作流" aria-label="刷新工作流" onClick={onRefresh}><RefreshCw size={16} className={loading ? 'spin' : ''} /></button>}
      />
      {error ? <div className="blocker"><FileText size={18} /><div><strong>工作流数据读取失败</strong><span>{error}</span></div></div> : null}
      <section className="workflow-summary" aria-label="工作流概览">
        <div><Workflow size={18} /><span><small>已安装</small><strong>{workflows.length}</strong></span></div>
        <div><CheckCircle2 size={18} /><span><small>已完成</small><strong>{succeeded}</strong></span></div>
        <div><Clock3 size={18} /><span><small>等待操作</small><strong>{waiting}</strong></span></div>
        <div><FileText size={18} /><span><small>Artifact</small><strong>{artifacts}</strong></span></div>
      </section>
      <div className="workflow-layout">
        <section className="card workflow-list-panel">
          <div className="card-header"><strong>已安装工作流</strong><Pill kind="neutral">{workflows.length}</Pill></div>
          <div className="workflow-list">
            {workflows.map(item => (
              <button
                type="button"
                key={item.package.id}
                className={selectedWorkflow?.package.id === item.package.id ? 'active' : ''}
                onClick={() => setSelectedWorkflowId(item.package.id)}
              >
                <span className="workflow-list-mark"><Workflow size={16} /></span>
                <span>
                  <strong>{item.package.name}</strong>
                  <small>v{item.package.version} · {item.package.steps.length} 步</small>
                </span>
                <Pill kind={item.enabled ? 'success' : 'neutral'}>{item.enabled ? '已启用' : '已停用'}</Pill>
              </button>
            ))}
            {!loading && workflows.length === 0 ? <EmptyState icon={Workflow} title="暂无工作流" text="安装 Workflow Package 后会显示在这里。" /> : null}
          </div>
        </section>
        <section className="card workflow-detail-panel">
          {selectedWorkflow ? (
            <>
              <div className="workflow-detail-head">
                <div>
                  <span>Workflow Package</span>
                  <h2>{selectedWorkflow.package.name}</h2>
                  <p>{selectedWorkflow.package.description}</p>
                </div>
                <div className="workflow-detail-actions">
                  <Pill kind={selectedWorkflow.enabled ? 'success' : 'neutral'}>{selectedWorkflow.enabled ? '可运行' : '已停用'}</Pill>
                  <button type="button" className="btn btn-primary" disabled={!selectedWorkflow.enabled} onClick={openStart}><Play size={14} />运行</button>
                </div>
              </div>
              <div className="workflow-meta-grid">
                <div><span>版本</span><strong>v{selectedWorkflow.package.version}</strong></div>
                <div><span>UI</span><strong>{selectedWorkflow.package.ui.mode}</strong></div>
                <div><span>Runtime</span><strong>{selectedWorkflow.package.supported_runtimes.length}</strong></div>
                <div><span>摘要</span><code>{selectedWorkflow.package_digest.slice(0, 12)}</code></div>
              </div>
              <div className="workflow-section">
                <div className="workflow-section-title"><strong>执行步骤</strong><span>{selectedWorkflow.package.steps.length} 步</span></div>
                <div className="workflow-step-list">
                  {selectedWorkflow.package.steps.map((step, index) => (
                    <div key={step.id}>
                      <span className="workflow-step-index">{String(index + 1).padStart(2, '0')}</span>
                      <span><strong>{step.title}</strong><small>{step.capability_id || '人工 / Runtime 步骤'}</small></span>
                      {step.approval_required ? <ShieldCheck size={15} aria-label="需要审批" /> : null}
                    </div>
                  ))}
                </div>
              </div>
              <div className="workflow-section">
                <div className="workflow-section-title"><strong>交付 Artifact</strong><span>{selectedWorkflow.package.artifacts.length} 类</span></div>
                <div className="workflow-artifact-list">
                  {selectedWorkflow.package.artifacts.map(artifact => (
                    <div key={artifact.id}><span><strong>{artifact.name}</strong><small>{artifact.artifact_type}</small></span><Pill kind={artifact.required ? 'warn' : 'neutral'}>{artifact.required ? '必需' : '可选'}</Pill></div>
                  ))}
                </div>
              </div>
            </>
          ) : <EmptyState icon={Workflow} title="请选择工作流" text="左侧列表用于查看包结构和交付契约。" />}
        </section>
        <section className="card workflow-runs-panel">
          <div className="card-header"><strong>最近运行</strong><Pill kind="neutral">{runs.length}</Pill></div>
          <div className="workflow-run-list">
            {runs.map(item => (
              <button type="button" key={item.run.run_id} className={selectedRunId === item.run.run_id ? 'active' : ''} onClick={() => openRun(item.run.run_id)}>
                <span><strong>{item.run.run_id}</strong><small>{formatTime(item.run.updated_at)}</small></span>
                <Pill kind={statusKind(item.run.status)}>{statusLabel(item.run.status)}</Pill>
              </button>
            ))}
            {!loading && runs.length === 0 ? <EmptyState icon={Clock3} title="暂无运行记录" text="工作流执行后会保留在本地 Ledger。" /> : null}
          </div>
          <div className="workflow-run-detail">
            {runLoading ? <div className="page-loading"><span className="spinner" />正在读取运行详情</div> : null}
            {runError ? <div className="workflow-inline-error">{runError}</div> : null}
            {runDetail ? (
              <>
                <div className="workflow-run-status">
                  <Pill kind={statusKind(runDetail.run.status)}>{statusLabel(runDetail.run.status)}</Pill>
                  <span>{runDetail.run.artifacts.length} Artifact · {runDetail.events.length} Event</span>
                </div>
                {waitingForFeedback ? (
                  <div className="workflow-run-actions">
                    <button
                      type="button"
                      className="btn btn-primary"
                      disabled={Boolean(actionBusy)}
                      onClick={() => void performRunAction(runDetail.run.run_id, 'resume', () => onResume(runDetail.run.run_id))}
                    >
                      <CheckCircle2 size={14} />{actionBusy === 'resume' ? '继续中' : '使用当前输入继续'}
                    </button>
                  </div>
                ) : runDetail.run.status === 'waiting' && runDetail.run.current_step_id ? (
                  <div className="workflow-run-actions">
                    <button
                      type="button"
                      className="btn btn-primary"
                      disabled={Boolean(actionBusy)}
                      onClick={() => void performRunAction(runDetail.run.run_id, 'approve', () => onApprove(runDetail.run.run_id, runDetail.run.current_step_id))}
                    >
                      <CheckCircle2 size={14} />{actionBusy === 'approve' ? '处理中' : '批准并继续'}
                    </button>
                    <button
                      type="button"
                      className="btn"
                      disabled={Boolean(actionBusy)}
                      onClick={() => void performRunAction(runDetail.run.run_id, 'reject', () => onReject(runDetail.run.run_id, runDetail.run.current_step_id))}
                    >
                      拒绝
                    </button>
                  </div>
                ) : null}
                {runDetail.run.status === 'running' || runDetail.run.status === 'queued' || runDetail.run.status === 'waiting' ? (
                  <div className="workflow-run-actions">
                    <button
                      type="button"
                      className="btn btn-danger-quiet"
                      disabled={Boolean(actionBusy)}
                      onClick={() => void performRunAction(runDetail.run.run_id, 'cancel', () => onCancel(runDetail.run.run_id))}
                    >
                      {actionBusy === 'cancel' ? '正在取消' : '取消运行'}
                    </button>
                  </div>
                ) : null}
                <div className="workflow-run-artifacts">
                  {runDetail.run.artifacts.map(artifact => (
                    <div key={artifact.artifact_id}><FileText size={14} /><span><strong>{artifact.name}</strong><small>{artifact.artifact_id}</small></span></div>
                  ))}
                </div>
              </>
            ) : null}
          </div>
        </section>
      </div>
      {startOpen && selectedWorkflow ? (
        <div className="modal-backdrop" role="presentation" onClick={event => { if (event.currentTarget === event.target) setStartOpen(false); }}>
          <section className="modal workflow-start-modal" role="dialog" aria-modal="true" aria-labelledby="workflow-start-title">
            <div className="workflow-start-head">
              <div><span>Workflow Package</span><h2 id="workflow-start-title">{selectedWorkflow.package.name}</h2></div>
              <button type="button" className="btn btn-icon" title="关闭" aria-label="关闭" onClick={() => setStartOpen(false)}><X size={16} /></button>
            </div>
            <div className="workflow-start-mode segmented-control" role="group" aria-label="输入模式">
              <button type="button" className={startMode === 'form' ? 'active' : ''} onClick={() => switchStartMode('form')}>表单</button>
              <button type="button" className={startMode === 'json' ? 'active' : ''} onClick={() => switchStartMode('json')}>JSON</button>
            </div>
            {startMode === 'form' ? (
              <div className="workflow-start-form">
                {selectedWorkflow.view?.sections.map(section => (
                  <section key={section.id}>
                    <h3>{section.title}</h3>
                    <div className="workflow-start-fields">
                      {(section.fields || []).map(rawField => {
                        const field = normalizeStartField(rawField);
                        const value = startForm[field.id];
                        const label = <span>{field.label}{field.required ? <em>必需</em> : null}</span>;
                        if (field.type === 'boolean') {
                          return (
                            <label key={field.id} className="workflow-start-toggle">
                              <input type="checkbox" checked={Boolean(value)} onChange={event => setStartField(field.id, event.target.checked)} />
                              <span>{field.label}</span>
                            </label>
                          );
                        }
                        if (field.type === 'select') {
                          return (
                            <label key={field.id}>
                              {label}
                              <select value={String(value || '')} onChange={event => setStartField(field.id, event.target.value)}>
                                <option value="">请选择</option>
                                {field.options.map(option => <option key={option} value={option}>{option}</option>)}
                              </select>
                            </label>
                          );
                        }
                        if (field.type === 'textarea' || field.type === 'list' || field.type === 'json') {
                          return (
                            <label key={field.id} className="workflow-start-field-wide">
                              {label}
                              <textarea
                                rows={field.type === 'list' ? 3 : 4}
                                value={String(value ?? '')}
                                placeholder={field.placeholder || (field.type === 'list' ? '每行一项' : '')}
                                onChange={event => setStartField(field.id, event.target.value)}
                              />
                            </label>
                          );
                        }
                        return (
                          <label key={field.id}>
                            {label}
                            <input
                              type={field.type === 'number' ? 'number' : 'text'}
                              value={String(value ?? '')}
                              placeholder={field.placeholder}
                              onChange={event => setStartField(field.id, event.target.value)}
                            />
                          </label>
                        );
                      })}
                    </div>
                  </section>
                ))}
                {!(selectedWorkflow.view?.sections || []).some(section => (section.fields || []).length) ? (
                  <div className="workflow-start-empty">该 Workflow 未声明结构化输入，可切换到 JSON 模式。</div>
                ) : null}
              </div>
            ) : (
              <textarea className="workflow-start-json" value={startInput} onChange={event => setStartInput(event.target.value)} spellCheck={false} />
            )}
            {startError ? <div className="workflow-inline-error">{startError}</div> : null}
            <div className="modal-actions">
              <button type="button" className="btn" onClick={() => setStartOpen(false)}>取消</button>
              <button type="button" className="btn btn-primary" disabled={startBusy} onClick={() => void submitStart()}><Play size={14} />{startBusy ? '正在启动' : '启动运行'}</button>
            </div>
          </section>
        </div>
      ) : null}
    </div>
  );
}
