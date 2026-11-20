import { OrganizationPicker } from './OrganizationPicker';
import type { WorkbenchAudience, WorkbenchSubmissionContext, WorkbenchSubmissionScope } from '../services/extensionSubmission';
import { useEffect, useId, useState } from 'react';
import { Globe2, Users, X } from 'lucide-react';
import { ModalSurface } from './ModalSurface';
import { organizationRef, type OrganizationDirectory, type OrganizationNode } from '../services/organizationDirectory';
import './WorkbenchAudienceDialog.css';

const messageFor = (error: unknown) => error instanceof Error ? error.message : String(error);

export function WorkbenchAudienceDialog({ name, version, github, update, published, load, directory, onConfirm, onClose }: {
  name: string;
  version: string;
  github?: boolean;
  update?: boolean;
  published?: boolean;
  directory: OrganizationDirectory;
  load(): Promise<WorkbenchSubmissionContext>;
  onConfirm(scope: WorkbenchSubmissionScope, revision?: number): Promise<void>;
  onClose(): void;
}) {
  const titleId = useId();
  const [context, setContext] = useState<WorkbenchSubmissionContext>();
  const [mode, setMode] = useState<WorkbenchAudience['mode']>('organization');
  const [choosing, setChoosing] = useState(false);
  const [department, setDepartment] = useState('');
  const [selected, setSelected] = useState(new Map<string, OrganizationNode>());
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState('');
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let live = true;
    void load().then(result => {
      if (!live) return;
      setContext(result);
      setMode(result.audience.mode);
      setDepartment(result.review?.department_id || (result.review_departments.length === 1 ? result.review_departments[0].id : ''));
      setChoosing(!result.audience.mode);
      setError('');
    }).catch(cause => { if (live) setError(messageFor(cause)); });
    return () => { live = false; };
  }, [load, attempt]);
  useEffect(() => {
    if (!context || context.audience.mode !== 'restricted') return;
    const refs = [...(context.audience.user_ids || []).map(id => `person:${id}`), ...(context.audience.department_ids || []).map(id => `department:${id}`)];
    let live = true;
    // Preserve references even when the directory is briefly unavailable.
    setSelected(new Map(refs.map(reference => {
      const boundary = reference.indexOf(':');
      return [reference, { id: reference.slice(boundary + 1), kind: reference.slice(0, boundary) as OrganizationNode['kind'], name: reference.slice(boundary + 1) }];
    })));
    const hydrate = async () => {
      let cursor: string | undefined;
      do {
        const page = await directory.resolve(refs, cursor);
        if (!live) return;
        setSelected(previous => new Map([...previous].map(([key, node]) => [key, page.nodes.find(item => organizationRef(item) === key) || node])));
        cursor = page.next_cursor;
      } while (cursor);
    };
    void hydrate().catch(cause => { if (live) setError(messageFor(cause)); });
    return () => { live = false; };
  }, [directory, context]);
  const toggle = (node: OrganizationNode) => setSelected(previous => {
    const next = new Map(previous);
    const reference = organizationRef(node);
    if (next.has(reference)) next.delete(reference); else next.set(reference, node);
    return next;
  });
  const count = (kind: OrganizationNode['kind']) => [...selected.values()].filter(node => node.kind === kind).length;
  const summary = mode === 'organization' ? '组织内公开' : mode === 'restricted' ? `${count('person')} 人 · ${count('department')} 个部门` : '请选择可见范围';
  const canSubmit = context && !!mode && (mode !== 'restricted' || selected.size > 0) && (update || context.review_departments.length < 2 || !!department);
  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (!canSubmit || submitting) return;
    setSubmitting(true);
    setError('');
    const audience: WorkbenchAudience = mode === 'organization' ? { mode } : {
      mode, user_ids: [...selected.values()].filter(node => node.kind === 'person').map(node => node.id), department_ids: [...selected.values()].filter(node => node.kind === 'department').map(node => node.id),
    };
    try { await onConfirm({ audience, review_department_id: department }, context.review?.revision ?? context.policy_revision); onClose(); }
    catch (cause) { setError(messageFor(cause)); }
    finally { setSubmitting(false); }
  }
  return <ModalSurface className={`workbench-audience-dialog${choosing ? ' is-expanded' : ''}`} titleId={titleId} onClose={() => { if (!submitting) onClose(); }}>
    <form onSubmit={submit}>
      <header><div><h2 id={titleId}>{update ? '修改可见范围' : github ? '确认扩展发布' : '提交到工作台'}</h2><p>{name} · v{version}</p></div><button type="button" className="btn btn-ghost" aria-label="关闭" onClick={onClose} disabled={submitting}><X size={18} /></button></header>
      <div className="workbench-audience-content">
        {!context && !error && <p role="status">正在读取提审配置…</p>}
        {context && <>
          <div className="workbench-audience-scope"><span className="workbench-audience-icon">{mode === 'restricted' ? <Users size={18} /> : <Globe2 size={18} />}</span><div><small>工作台可见范围</small><strong>{summary}</strong></div><button type="button" className="btn" onClick={() => setChoosing(current => !current)} aria-expanded={choosing}>{choosing ? '收起' : '修改'}</button></div>
          {choosing && <fieldset className="workbench-audience-modes"><legend>选择范围</legend><label><input type="radio" name="audience-mode" checked={mode === 'organization'} onChange={() => setMode('organization')} />组织内公开</label><label><input type="radio" name="audience-mode" checked={mode === 'restricted'} onChange={() => setMode('restricted')} />指定人员和部门</label></fieldset>}
          {choosing && mode === 'restricted' && <OrganizationPicker directory={directory} selected={selected} onToggle={toggle} onClear={() => setSelected(new Map())} />}
          {!update && context.review_departments.length > 1 && <label className="workbench-review-department">审核归属<select value={department} onChange={event => setDepartment(event.target.value)}><option value="">选择直属部门</option>{context.review_departments.map(item => <option key={item.id} value={item.id}>{item.name}</option>)}</select></label>}
          {published && <p>范围变更由归属部门审核，通过前继续按原范围分发。</p>}
          {github && <p className="workbench-github-note">GitHub 制品按仓库权限访问。公开仓库允许所有人获取。</p>}
        </>}
        {error && <div className="organization-picker-message" role="alert"><span>{error}</span>{!context && <button type="button" className="btn" onClick={() => { setError(''); setAttempt(value => value + 1); }}>重试</button>}</div>}
      </div>
      <footer><span title={summary}>{context ? summary : ''}</span><button type="button" className="btn" onClick={onClose} disabled={submitting}>取消</button><button type="submit" className="btn btn-primary" disabled={!canSubmit || submitting}>{submitting ? '正在提交…' : published ? '提交范围变更' : update ? '保存范围' : github ? '确认发布' : '提交审核'}</button></footer>
    </form>
  </ModalSurface>;
}
