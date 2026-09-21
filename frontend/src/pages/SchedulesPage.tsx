import { useEffect, useMemo, useState } from 'react';
import { BookOpen, CalendarClock, Clock3, FolderOpen, RefreshCw, Store, Trash2, Workflow } from 'lucide-react';
import { EmptyState, PageHeader, Pill } from '../components/Common';
import type { Schedule, ScheduleInput, ScheduleList, SkillRun, WorkflowCenterSnapshot, WorkflowPreflight, WorkflowRunPreset } from '../services/agentApi';
import {
  fieldsToInput,
  initialFormValues,
  invalidOptionValue,
  jsonToFormValues,
  normalizeStartField,
  type WorkflowStartField,
} from './workflowStartForm';

type SchedulesPageProps = {
  snapshot: WorkflowCenterSnapshot | null;
  skills: Array<{ id: string; name: string; version: string }>;
  onRefreshWorkflows: () => void;
  onLoadSchedules: () => Promise<ScheduleList>;
  onSaveSchedule: (input: ScheduleInput) => Promise<void>;
  onDeleteSchedule: (id: string) => Promise<void>;
  onRunTarget: (kind: string, targetId: string, input: Record<string, unknown>) => Promise<void>;
  onRunSkill: (skillId: string, input: Record<string, unknown>) => Promise<void>;
  onLoadWorkflowPresets: (workflowId: string) => Promise<{ presets: WorkflowRunPreset[] }>;
  onPreflightWorkflow: (workflowId: string, input: Record<string, unknown>) => Promise<WorkflowPreflight>;
  onLoadSkillRuns: (limit?: number) => Promise<{ runs: SkillRun[] }>;
  onRevealSkillRun: (runId: string) => Promise<void>;
  /** 从工作流页跳过来时预选的 Workflow 目标。 */
  presetTargetId: string;
  onOpenExtensions: () => void;
};

type Draft = {
  id: string;
  kind: string;
  targetId: string;
  cron: string;
  entrypoint: string;
  exitpoint: string;
  enabled: boolean;
  input: string;
  inputMode: 'form' | 'json';
  form: Record<string, unknown>;
  presetId: string;
};

const EMPTY_DRAFT: Draft = {
  id: '',
  kind: 'workflow',
  targetId: '',
  cron: '0 9 * * *',
  entrypoint: '',
  exitpoint: '',
  enabled: true,
  input: '{}',
  inputMode: 'json',
  form: {},
  presetId: '',
};

/** 常见 cron 的中文说明；命中不了就只显示表达式，不猜语义。 */
export function cronDescription(cron: string) {
  const presets: Record<string, string> = {
    '* * * * *': '每分钟',
    '*/5 * * * *': '每 5 分钟',
    '*/15 * * * *': '每 15 分钟',
    '*/30 * * * *': '每 30 分钟',
    '0 * * * *': '每小时整点',
    '0 9 * * *': '每天 09:00',
    '0 18 * * *': '每天 18:00',
    '30 9 * * 1-5': '工作日 09:30',
    '0 9 * * 1': '每周一 09:00',
    '0 9 1 * *': '每月 1 日 09:00',
  };
  return presets[cron.trim().replace(/\s+/g, ' ')] || '';
}

const CRON_PRESETS: Array<{ cron: string; label: string }> = [
  { cron: '0 9 * * *', label: '每天 09:00' },
  { cron: '30 9 * * 1-5', label: '工作日 09:30' },
  { cron: '0 * * * *', label: '每小时' },
  { cron: '*/15 * * * *', label: '每 15 分钟' },
  { cron: '0 9 * * 1', label: '每周一 09:00' },
];

function formatTime(value: string) {
  if (!value) return '--';
  const numeric = Number(value);
  const date = Number.isFinite(numeric) && numeric > 0 ? new Date(numeric * 1000) : new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

function targetLabel(kind: string) {
  return ({ workflow: '工作流', skill: '技能' } as Record<string, string>)[kind] || kind;
}

function runStatusLabel(status?: string) {
  return ({ queued: '排队中', running: '运行中', waiting: '等待处理', succeeded: '已完成', failed: '失败', canceled: '已取消' } as Record<string, string>)[status || ''] || '尚未运行';
}

export function SchedulesPage({ snapshot, skills, onRefreshWorkflows, onLoadSchedules, onSaveSchedule, onDeleteSchedule, onRunTarget, onRunSkill, onLoadSkillRuns, onRevealSkillRun, onLoadWorkflowPresets, onPreflightWorkflow, presetTargetId, onOpenExtensions }: SchedulesPageProps) {
  const workflows = snapshot?.workflows || [];
  const [list, setList] = useState<ScheduleList | null>(null);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState('');
  const [error, setError] = useState('');
  const [selectedId, setSelectedId] = useState('');
  const [draft, setDraft] = useState<Draft>(EMPTY_DRAFT);
  const [skillRuns, setSkillRuns] = useState<SkillRun[]>([]);
  const [presets, setPresets] = useState<WorkflowRunPreset[]>([]);
  const [preflight, setPreflight] = useState<WorkflowPreflight | null>(null);
  const [preflightBusy, setPreflightBusy] = useState(false);
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});

  async function loadSkillRuns() {
    try {
      const result = await onLoadSkillRuns(5);
      setSkillRuns(result.runs || []);
    } catch {
      setSkillRuns([]);
    }
  }

  const selected = useMemo(() => (list?.schedules || []).find(item => item.id === selectedId) || null, [list, selectedId]);
  const targetWorkflow = useMemo(() => workflows.find(item => item.package.id === draft.targetId) || null, [draft.targetId, workflows]);
  const workflowFields = useMemo(() => (targetWorkflow?.view?.sections || []).flatMap(section => (section.fields || []).map(normalizeStartField)), [targetWorkflow]);
  const entrypoints = ((targetWorkflow?.package as unknown as { entrypoints?: Array<{ id: string; label?: string }> } | null)?.entrypoints) || [];
  const exits = ((targetWorkflow?.package as unknown as { exits?: Array<{ id: string; label?: string }> } | null)?.exits) || [];
  const enabledCount = (list?.schedules || []).filter(item => item.enabled).length;
  const failedCount = (list?.schedules || []).filter(item => item.last_status === 'failed').length;

  function targetName(kind: string, targetId: string) {
    if (kind === 'workflow') return workflows.find(item => item.package.id === targetId)?.package.name || targetId;
    if (kind === 'skill') return skills.find(item => item.id === targetId)?.name || targetId;
    return targetId;
  }

  function fieldsFor(workflowId: string) {
    const workflow = workflows.find(item => item.package.id === workflowId);
    return (workflow?.view?.sections || []).flatMap(section => (section.fields || []).map(normalizeStartField));
  }

  function inputModeFor(kind: string, fields: WorkflowStartField[]) {
    return kind === 'workflow' && fields.length ? 'form' as const : 'json' as const;
  }

  function applySchedule(item: Schedule) {
    setSelectedId(item.id);
    setError('');
    const fields = fieldsFor(item.target_id);
    const input = item.input || {};
    setDraft({
      id: item.id,
      kind: item.kind || 'workflow',
      targetId: item.target_id,
      cron: item.cron,
      entrypoint: item.execution?.entrypoint || '',
      exitpoint: item.execution?.exitpoint || '',
      enabled: item.enabled,
      input: JSON.stringify(input, null, 2),
      inputMode: inputModeFor(item.kind || 'workflow', fields),
      form: fields.length ? jsonToFormValues(fields, input) : {},
      presetId: item.preset_id || '',
    });
    setFieldErrors({});
    setPreflight(null);
  }

  async function load(preferId?: string) {
    setLoading(true);
    setError('');
    try {
      const next = await onLoadSchedules();
      setList(next);
      const wanted = next.schedules.find(item => item.id === (preferId || selectedId));
      const fallback = wanted || next.schedules[0];
      if (fallback) applySchedule(fallback);
      else if (!presetTargetId) {
        setSelectedId('');
        setDraft(EMPTY_DRAFT);
      }
    } catch {
      setError('读取定时任务失败，请检查本机服务后重试');
    } finally {
      setLoading(false);
    }
    await loadSkillRuns();
  }

  useEffect(() => {
    void load();
  }, []);

  useEffect(() => {
    if (draft.kind !== 'workflow' || !draft.targetId) {
      setPresets([]);
      return;
    }
    let disposed = false;
    void onLoadWorkflowPresets(draft.targetId)
      .then(result => { if (!disposed) setPresets(result.presets || []); })
      .catch(() => { if (!disposed) setPresets([]); });
    return () => { disposed = true; };
  }, [draft.kind, draft.targetId, onLoadWorkflowPresets]);

  // 从工作流页“加定时计划”跳过来时，预选目标并清空选中计划。
  useEffect(() => {
    if (!presetTargetId) return;
    setSelectedId('');
    const fields = fieldsFor(presetTargetId);
    const form = initialFormValues(fields);
    setDraft(current => ({
      ...EMPTY_DRAFT,
      kind: 'workflow',
      targetId: presetTargetId,
      inputMode: fields.length ? 'form' : 'json',
      form,
      input: JSON.stringify(fields.length ? fieldsToInput(fields, form) : {}, null, 2),
    }));
    setFieldErrors({});
    setPreflight(null);
  }, [presetTargetId]);

  function beginNew() {
    setSelectedId('');
    setError('');
    const targetId = draft.kind === 'skill'
      ? (skills[0]?.id || '')
      : (draft.targetId || workflows[0]?.package.id || '');
    const fields = draft.kind === 'workflow' ? fieldsFor(targetId) : [];
    const form = initialFormValues(fields);
    setDraft({
      ...EMPTY_DRAFT,
      kind: draft.kind,
      targetId,
      inputMode: inputModeFor(draft.kind, fields),
      form,
      input: JSON.stringify(fields.length ? fieldsToInput(fields, form) : {}, null, 2),
    });
    setFieldErrors({});
    setPreflight(null);
  }

  function buildInput(): Record<string, unknown> {
    if (draft.kind === 'workflow' && draft.inputMode === 'form' && workflowFields.length) {
      const missing = workflowFields.find(field => field.required && !String(draft.form[field.id] ?? '').trim());
      if (missing) {
        setFieldErrors({ [missing.id]: `${missing.label}不能为空` });
        throw new Error(`${missing.label}不能为空`);
      }
      const invalid = workflowFields
        .map(field => ({ field, invalidValue: invalidOptionValue(field, draft.form[field.id]) }))
        .find(item => item.invalidValue);
      if (invalid) {
        setFieldErrors({ [invalid.field.id]: `${invalid.field.label}不支持「${invalid.invalidValue}」` });
        throw new Error(`${invalid.field.label}不支持「${invalid.invalidValue}」`);
      }
      return fieldsToInput(workflowFields, draft.form);
    }
    try {
      const parsed = draft.input.trim() ? JSON.parse(draft.input) : {};
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('not-object');
      return parsed as Record<string, unknown>;
    } catch {
      throw new Error('运行输入不是合法 JSON 对象');
    }
  }

  async function runWorkflowPreflight(input: Record<string, unknown>) {
    if (draft.kind !== 'workflow' || !draft.targetId) return true;
    setPreflightBusy(true);
    try {
      const preflightInput: Record<string, unknown> = { ...input };
      if (draft.entrypoint.trim() || draft.exitpoint.trim()) {
        preflightInput.execution = { entrypoint: draft.entrypoint.trim(), exitpoint: draft.exitpoint.trim() };
      }
      const report = await onPreflightWorkflow(draft.targetId, preflightInput);
      setPreflight(report);
      if (!report.ready) {
        setError(report.blockers[0] || '运行前检查未通过');
        return false;
      }
      return true;
    } catch {
      setPreflight(null);
      setError('运行前检查失败，请检查工作流依赖和连接状态');
      return false;
    } finally {
      setPreflightBusy(false);
    }
  }

  async function save() {
    let parsed: Record<string, unknown>;
    try {
      parsed = buildInput();
    } catch (inputError) {
      setError(inputError instanceof Error ? inputError.message : '请检查运行输入');
      return;
    }
    if (!draft.targetId) {
      setError('请选择要定时运行的目标');
      return;
    }
    if (!draft.cron.trim()) {
      setError('请填写 cron 表达式（分 时 日 月 周）');
      return;
    }
    if (draft.kind === 'workflow' && !(await runWorkflowPreflight(parsed))) return;
    setBusy('save');
    setError('');
    try {
      await onSaveSchedule({
        id: draft.id || undefined,
        kind: draft.kind,
        target_id: draft.targetId,
        preset_id: draft.kind === 'workflow' ? (draft.presetId || undefined) : undefined,
        cron: draft.cron.trim(),
        input: parsed,
        execution: { entrypoint: draft.entrypoint.trim(), exitpoint: draft.exitpoint.trim() },
        enabled: draft.enabled,
      });
      await load(draft.id);
    } catch {
      setError('保存失败：请确认 cron 表达式、目标是否已启用/安装');
    } finally {
      setBusy('');
    }
  }

  async function remove(id: string) {
    setBusy(`delete:${id}`);
    setError('');
    try {
      await onDeleteSchedule(id);
      if (selectedId === id) setSelectedId('');
      await load();
    } catch {
      setError('删除定时任务失败');
    } finally {
      setBusy('');
    }
  }

  async function runNow(item: Schedule) {
    setBusy(`run:${item.id}`);
    setError('');
    try {
      const input: Record<string, unknown> = { ...(item.input || {}) };
      if (item.kind === 'skill') {
        await onRunSkill(item.target_id, input);
        await loadSkillRuns();
        return;
      }
      if (item.execution?.entrypoint || item.execution?.exitpoint) {
        input.execution = { entrypoint: item.execution?.entrypoint || '', exitpoint: item.execution?.exitpoint || '' };
      }
      await onRunTarget(item.kind, item.target_id, input);
    } catch (runError) {
      setError(runError instanceof Error ? runError.message : '立即运行失败，请查看运行记录');
    } finally {
      setBusy('');
    }
  }

  function selectPreset(presetId: string) {
    if (!presetId) {
      setDraft(current => ({ ...current, presetId: '' }));
      setPreflight(null);
      return;
    }
    const preset = presets.find(item => item.id === presetId);
    if (!preset) return;
    const fields = fieldsFor(preset.workflow_id);
    const input = preset.input || {};
    setDraft(current => ({
      ...current,
      presetId,
      targetId: preset.workflow_id,
      entrypoint: preset.entrypoint || '',
      exitpoint: preset.exitpoint || '',
      input: JSON.stringify(input, null, 2),
      inputMode: fields.length ? 'form' : 'json',
      form: fields.length ? jsonToFormValues(fields, input) : {},
    }));
    setFieldErrors({});
    setPreflight(null);
  }

  function switchInputMode(mode: 'form' | 'json') {
    if (mode === draft.inputMode) return;
    if (mode === 'json') {
      try {
        const input = fieldsToInput(workflowFields, draft.form);
        setDraft(current => ({ ...current, inputMode: 'json', input: JSON.stringify(input, null, 2) }));
        setFieldErrors({});
      } catch {
        setError('结构化输入暂时无法转换为 JSON');
      }
      return;
    }
    try {
      const parsed = draft.input.trim() ? JSON.parse(draft.input) : {};
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('not-object');
      setDraft(current => ({ ...current, inputMode: 'form', form: jsonToFormValues(workflowFields, parsed) }));
      setFieldErrors({});
    } catch {
      setError('JSON 需要是有效对象，才能切换回结构化输入');
    }
  }

  function setWorkflowField(field: WorkflowStartField, value: unknown) {
    setDraft(current => ({ ...current, form: { ...current.form, [field.id]: value } }));
    setFieldErrors(current => {
      if (!current[field.id]) return current;
      const next = { ...current };
      delete next[field.id];
      return next;
    });
    setPreflight(null);
  }

  return (
    <div className="workflow-page">
      <PageHeader
        title="定时任务"
        description="按计划自动运行工作流和技能。"
        actions={
          <button className="btn btn-icon" title="刷新定时任务" aria-label="刷新定时任务" onClick={() => { onRefreshWorkflows(); void load(); }}>
            <RefreshCw size={16} className={loading ? 'spin' : ''} />
          </button>
        }
      />
      <section className="workflow-summary" aria-label="定时任务概览">
        <div><CalendarClock size={18} /><span><small>任务总数</small><strong>{list?.schedules.length || 0}</strong></span></div>
        <div className={enabledCount ? '' : 'attention'}><Clock3 size={18} /><span><small>已启用</small><strong>{enabledCount}</strong></span></div>
        <div className={failedCount ? 'attention' : ''}><Clock3 size={18} /><span><small>上次失败</small><strong>{failedCount}</strong></span></div>
        <div><Workflow size={18} /><span><small>可运行项</small><strong>{workflows.length + skills.length}</strong></span></div>
      </section>
      <div className="workflow-layout">
        <section className="card workflow-list-panel">
          <div className="card-header">
            <strong>定时任务</strong>
            <span className="workflow-library-actions">
              <Pill kind="neutral">{list?.schedules.length || 0}</Pill>
              <button type="button" className="btn btn-primary" onClick={beginNew}><CalendarClock size={14} />新建定时任务</button>
            </span>
          </div>
          <div className="workflow-list">
            {(list?.schedules || []).map(item => (
              <button type="button" key={item.id} className={selectedId === item.id ? 'active' : ''} onClick={() => applySchedule(item)}>
                <span className="workflow-list-mark"><CalendarClock size={16} /></span>
                <span>
                  <strong>{targetName(item.kind, item.target_id) || item.id}</strong>
                  <small>{targetLabel(item.kind)}{item.preset_id ? ` · 使用预设` : ''}</small>
                  <small>{cronDescription(item.cron) || item.cron} · 下次 {formatTime(item.next_run_at)}</small>
                </span>
                <Pill kind={!item.enabled ? 'neutral' : item.last_status === 'failed' ? 'danger' : 'success'}>
                  {!item.enabled ? '已暂停' : item.last_status === 'failed' ? '上次失败' : '已启用'}
                </Pill>
              </button>
            ))}
            {!loading && (list?.schedules || []).length === 0 ? (
              <div className="workflow-library-empty">
                <EmptyState icon={CalendarClock} title="还没有定时任务" text="新建后会按计划自动运行，与手动启动相同。" />
                <button type="button" className="btn btn-primary" onClick={beginNew}><CalendarClock size={14} />新建定时任务</button>
              </div>
            ) : null}
          </div>
        </section>
        <section className="card workflow-detail-panel">
          {error ? <div className="workflow-inline-error">{error}</div> : null}
          <div className="workflow-detail-head">
            <div>
              <span>定时规则</span>
              <h2>{draft.targetId ? targetName(draft.kind, draft.targetId) : '新建定时任务'}</h2>
              <p>按 {list?.timezone || '本机'} 时区运行。</p>
            </div>
            <div className="workflow-detail-actions">
              <Pill kind={draft.enabled ? 'success' : 'neutral'}>{draft.enabled ? '已启用' : '已暂停'}</Pill>
            </div>
          </div>
          <div className="workflow-start-form schedule-form">
            <section>
              <h3>计划</h3>
              <div className="workflow-start-fields">
                <label>
                  <span>任务名称（留空自动生成）</span>
                  <input value={draft.id} placeholder="daily-tech-radar" spellCheck={false} onChange={event => setDraft(current => ({ ...current, id: event.target.value }))} />
                </label>
                <label>
                  <span>目标类型</span>
                  <select value={draft.kind} onChange={event => {
                    const kind = event.target.value;
                    const targetId = kind === 'skill' ? (skills[0]?.id || '') : (workflows[0]?.package.id || '');
                    const fields = kind === 'workflow' ? fieldsFor(targetId) : [];
                    const form = initialFormValues(fields);
                    setDraft(current => ({ ...current, kind, targetId, presetId: '', inputMode: inputModeFor(kind, fields), form, input: JSON.stringify(fields.length ? fieldsToInput(fields, form) : {}, null, 2) }));
                    setFieldErrors({});
                    setPreflight(null);
                  }}>
                    {(list?.target_kinds || ['workflow']).map(kind => <option key={kind} value={kind}>{targetLabel(kind)}</option>)}
                  </select>
                </label>
                <label className="workflow-start-field-wide">
                  <span>目标</span>
                  {draft.kind === 'workflow' ? (
                    <select value={draft.targetId} onChange={event => {
                      const targetId = event.target.value;
                      const fields = fieldsFor(targetId);
                      const form = initialFormValues(fields);
                      setDraft(current => ({ ...current, targetId, presetId: '', inputMode: fields.length ? 'form' : 'json', form, input: JSON.stringify(fields.length ? fieldsToInput(fields, form) : {}, null, 2) }));
                      setFieldErrors({});
                      setPreflight(null);
                    }}>
                      <option value="">请选择工作流</option>
                      {workflows.map(item => <option key={item.package.id} value={item.package.id}>{item.package.name}</option>)}
                    </select>
                  ) : draft.kind === 'skill' ? (
                    <select value={draft.targetId} onChange={event => { setDraft(current => ({ ...current, targetId: event.target.value })); setPreflight(null); }}>
                      <option value="">请选择技能</option>
                      {skills.map(item => <option key={item.id} value={item.id}>{item.name} · v{item.version}</option>)}
                    </select>
                  ) : (
                    <input value={draft.targetId} spellCheck={false} onChange={event => setDraft(current => ({ ...current, targetId: event.target.value }))} />
                  )}
                </label>
                {draft.kind === 'skill' && skills.length === 0 ? (
                  <div className="workflow-start-empty workflow-start-field-wide">
                    还没有已安装的技能。<button type="button" className="btn" onClick={onOpenExtensions}><Store size={13} />去市场安装</button>
                  </div>
                ) : null}
                {draft.kind === 'workflow' && workflows.length === 0 ? (
                  <div className="workflow-start-empty workflow-start-field-wide">
                    还没有可用的工作流。<button type="button" className="btn" onClick={onOpenExtensions}><Store size={13} />去市场安装</button>
                  </div>
                ) : null}
                {draft.kind === 'workflow' ? (
                  <label>
                    <span>启动预设</span>
                    <select value={draft.presetId} onChange={event => selectPreset(event.target.value)}>
                      <option value="">手动配置（保存当前输入快照）</option>
                      {presets.map(preset => <option key={preset.id} value={preset.id}>{preset.label || preset.id}</option>)}
                    </select>
                    <small className="workflow-start-hint">选择预设会自动填充启动参数。</small>
                  </label>
                ) : null}
                <label className="workflow-start-field-wide">
                  <span>运行计划（分 时 日 月 周）{cronDescription(draft.cron) ? ` · ${cronDescription(draft.cron)}` : ''}</span>
                  <input value={draft.cron} placeholder="0 9 * * *" spellCheck={false} onChange={event => setDraft(current => ({ ...current, cron: event.target.value }))} />
                </label>
                <div className="workflow-start-field-wide schedule-presets">
                  {CRON_PRESETS.map(preset => (
                    <button type="button" key={preset.cron} className="btn" onClick={() => setDraft(current => ({ ...current, cron: preset.cron }))}>{preset.label}</button>
                  ))}
                </div>
                {draft.kind === 'workflow' ? (
                  <>
                    <label>
                      <span>入口（多入口时必填）</span>
                      <select value={draft.entrypoint} onChange={event => setDraft(current => ({ ...current, entrypoint: event.target.value }))}>
                        <option value="">按工作流默认</option>
                        {entrypoints.map(entry => <option key={entry.id} value={entry.id}>{entry.label || entry.id}</option>)}
                      </select>
                    </label>
                    <label>
                      <span>出口</span>
                      <select value={draft.exitpoint} onChange={event => setDraft(current => ({ ...current, exitpoint: event.target.value }))}>
                        <option value="">按工作流默认</option>
                        {exits.map(exit => <option key={exit.id} value={exit.id}>{exit.label || exit.id}</option>)}
                      </select>
                    </label>
                  </>
                ) : null}
                {draft.kind === 'workflow' && workflowFields.length ? (
                  <div className="workflow-start-field-wide schedule-workflow-input">
                    <div className="workflow-start-mode segmented-control" role="group" aria-label="工作流输入模式">
                      <button type="button" className={draft.inputMode === 'form' ? 'active' : ''} onClick={() => switchInputMode('form')}>结构化输入</button>
                      <button type="button" className={draft.inputMode === 'json' ? 'active' : ''} onClick={() => switchInputMode('json')}>JSON</button>
                    </div>
                    {draft.inputMode === 'form' ? (
                      <div className="workflow-start-form schedule-workflow-fields">
                        {targetWorkflow?.view?.sections.map(section => (
                          <section key={section.id}>
                            <h3>{section.title}</h3>
                            <div className="workflow-start-fields">
                              {(section.fields || []).map(rawField => {
                                const field = normalizeStartField(rawField);
                                const value = draft.form[field.id];
                                const fieldError = fieldErrors[field.id];
                                const label = <span>{field.label}{field.required ? <em>必需</em> : null}</span>;
                                const footer = <>{fieldError ? <small className="workflow-start-field-error">{fieldError}</small> : null}{field.hint ? <small className="workflow-start-hint">{field.hint}</small> : null}</>;
                                const fieldClass = field.type === 'textarea' || field.type === 'list' || field.type === 'json' || field.span === 'full' ? 'workflow-start-field-wide' : '';
                                if (field.type === 'boolean') {
                                  return <label key={field.id} className="workflow-start-toggle"><input type="checkbox" checked={Boolean(value)} onChange={event => setWorkflowField(field, event.target.checked)} /><span>{field.label}</span></label>;
                                }
                                if (field.type === 'select') {
                                  return <label key={field.id} className={fieldClass}>{label}<select value={String(value ?? '')} onChange={event => setWorkflowField(field, event.target.value)}><option value="">请选择</option>{field.options.map(option => <option key={option} value={option}>{option}</option>)}</select>{footer}</label>;
                                }
                                if (field.type === 'textarea' || field.type === 'list' || field.type === 'json') {
                                  return <label key={field.id} className={fieldClass}>{label}<textarea rows={field.type === 'list' ? 3 : 4} value={String(value ?? '')} placeholder={field.placeholder || (field.type === 'list' ? '每行一项' : '')} onChange={event => setWorkflowField(field, event.target.value)} />{footer}</label>;
                                }
                                return <label key={field.id} className={fieldClass}>{label}<input type={field.type === 'number' ? 'number' : 'text'} value={String(value ?? '')} placeholder={field.placeholder} onChange={event => setWorkflowField(field, event.target.value)} />{footer}</label>;
                              })}
                            </div>
                          </section>
                        ))}
                      </div>
                    ) : (
                      <textarea className="workflow-start-json" value={draft.input} spellCheck={false} rows={8} onChange={event => { setDraft(current => ({ ...current, input: event.target.value })); setPreflight(null); }} />
                    )}
                  </div>
                ) : (
                  <label className="workflow-start-field-wide">
                    <span>
                      运行输入（JSON）
                      {draft.kind === 'skill' ? ' · 技能需要 task，可加 workspace_root / timeout_seconds' : ''}
                    </span>
                    <textarea
                      value={draft.input}
                      spellCheck={false}
                      rows={6}
                      placeholder={draft.kind === 'skill' ? '{"task":"本次要让技能做什么"}' : '{}'}
                      onChange={event => { setDraft(current => ({ ...current, input: event.target.value })); setPreflight(null); }}
                    />
                  </label>
                )}
                <label className="workflow-start-toggle">
                  <input type="checkbox" checked={draft.enabled} onChange={event => setDraft(current => ({ ...current, enabled: event.target.checked }))} />
                  <span>启用该任务</span>
                </label>
              </div>
            </section>
            {draft.kind === 'workflow' ? (
              <section className={`workflow-preflight schedule-preflight${preflight?.ready ? ' ready' : preflight ? ' blocked' : ''}`}>
                <div className="workflow-preflight-head">
                  <span><strong>{preflight ? (preflight.ready ? '运行前检查通过' : '运行前检查未通过') : '保存前检查运行条件'}</strong><small>{preflight ? (preflight.ready ? '保存后将按当前参数运行。' : (preflight.blockers[0] || '请先处理阻塞项。')) : '提前检查依赖和凭据，避免任务执行失败。'}</small></span>
                  <button type="button" className="btn" disabled={preflightBusy || !draft.targetId} onClick={() => { try { const input = buildInput(); void runWorkflowPreflight(input); } catch (inputError) { setError(inputError instanceof Error ? inputError.message : '请检查工作流输入'); } }}>{preflightBusy ? '检查中' : '运行前检查'}</button>
                </div>
                {preflight ? <div className="workflow-preflight-grid"><span>功能 {preflight.capabilities.filter(item => item.available).length}/{preflight.capabilities.length}</span><span>技能 {preflight.skills.filter(item => item.available).length}/{preflight.skills.length}</span><span>运行环境 {preflight.runtimes.filter(item => item.available).length}/{preflight.runtimes.length}</span><span>连接 {preflight.connectors.filter(item => item.available && item.health_status === 'passed').length}/{preflight.connectors.length}</span></div> : null}
                {preflight && (preflight.blockers.length || preflight.warnings.length) ? <details className="schedule-preflight-details" open={!preflight.ready}><summary>{preflight.blockers.length ? `${preflight.blockers.length} 个阻塞项` : `${preflight.warnings.length} 条提示`}</summary>{preflight.blockers.map(message => <p key={`blocker:${message}`}><strong>阻塞</strong>{message}</p>)}{preflight.warnings.map(message => <p key={`warning:${message}`}><strong>提示</strong>{message}</p>)}</details> : null}
              </section>
            ) : null}
            <div className="schedule-actions">
              <button type="button" className="btn btn-primary" disabled={Boolean(busy) || preflightBusy} onClick={() => void save()}>{busy === 'save' ? '保存中' : '保存定时任务'}</button>
              {selected ? (
                <>
                  <button type="button" className="btn" disabled={Boolean(busy)} onClick={() => void runNow(selected)}>{busy.startsWith('run:') ? '启动中' : '立即运行'}</button>
                  <button type="button" className="btn btn-danger-quiet" disabled={Boolean(busy)} onClick={() => void remove(selected.id)}><Trash2 size={14} />{busy.startsWith('delete:') ? '删除中' : '删除任务'}</button>
                </>
              ) : null}
            </div>
            {selected ? (
              <section className="workflow-section">
                <div className="workflow-section-title"><strong>执行情况</strong><span>{runStatusLabel(selected.last_status)}</span></div>
                <div className="workflow-run-context">
                  <div><span>下次执行</span><strong>{formatTime(selected.next_run_at)}</strong></div>
                  <div><span>上次执行</span><strong>{formatTime(selected.last_run_at)}</strong></div>
                  <div><span>上次结果</span><strong>{runStatusLabel(selected.last_status)}</strong></div>
                  <div><span>任务名称</span><strong>{selected.id}</strong></div>
                </div>
                {selected.kind === 'skill' && selected.last_run_id ? (
                  <div className="schedule-actions">
                    <button type="button" className="btn" onClick={() => void onRevealSkillRun(selected.last_run_id)}>
                      <FolderOpen size={14} />打开上次结果
                    </button>
                  </div>
                ) : null}
                {selected.last_error ? <div className="workflow-inline-error">{selected.last_error}</div> : null}
              </section>
            ) : null}
            {draft.kind === 'skill' ? (
              <section className="workflow-section">
                <div className="workflow-section-title">
                  <strong>最近的技能运行</strong>
                  <button type="button" className="btn btn-icon" title="刷新技能运行" aria-label="刷新技能运行" onClick={() => void loadSkillRuns()}><RefreshCw size={14} /></button>
                </div>
                <div className="workflow-run-artifacts">
                  {skillRuns.map(run => (
                    <div key={run.run_id}>
                      <BookOpen size={14} />
                      <span>
                        <strong>{run.skill_name} · {run.status === 'succeeded' ? '已完成' : run.status === 'running' ? '运行中' : '失败'}</strong>
                        <small>{run.task} · {formatTime(run.started_at)}{run.duration_seconds ? ` · ${run.duration_seconds}s` : ''}</small>
                        {run.error ? <small>{run.error}</small> : null}
                      </span>
                      <button type="button" className="btn btn-icon" title="打开结果" aria-label={`打开 ${run.run_id} 的结果`} onClick={() => void onRevealSkillRun(run.run_id)}><FolderOpen size={14} /></button>
                    </div>
                  ))}
                  {skillRuns.length === 0 ? <div className="workflow-start-empty">还没有技能运行记录。</div> : null}
                </div>
              </section>
            ) : null}
          </div>
        </section>
      </div>
    </div>
  );
}
