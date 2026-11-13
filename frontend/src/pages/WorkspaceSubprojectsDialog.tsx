import { useState } from 'react';
import { X } from 'lucide-react';
import { agentApi, type WorkspaceRegistryBinding, type WorkspaceRegistryProject, type WorkspaceSubprojectInput, type WorkspaceSubprojectPreview } from '../services/agentApi';
import { ModalSurface } from '../components/ModalSurface';
import { WorkspaceFeedback, workspaceIssue, type WorkspaceIssue } from './WorkspaceFeedback';
import { projectDisplayName } from './workspaceProjectModel';

export function WorkspaceSubprojectsDialog({ parent, paths, revision, initialId, onClose, onApplied }: {
  parent: WorkspaceRegistryProject; paths: WorkspaceRegistryBinding[]; revision: string;
  initialId?: string; onClose: () => void; onApplied: (preview: WorkspaceSubprojectPreview) => void;
}) {
  const [rows, setRows] = useState(() => paths.map(path => ({ path, selected: path.id === initialId, name: '', aliases: path.path.replace(/\\/g, '/').split('/').filter(Boolean).slice(-1)[0] || '', kind: 'product' })));
  const [parentName, setParentName] = useState(projectDisplayName(parent));
  const [prepared, setPrepared] = useState<{ input: WorkspaceSubprojectInput; preview: WorkspaceSubprojectPreview } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<WorkspaceIssue | null>(null);
  const selected = rows.filter(row => row.selected);
  const update = (id: string, patch: Partial<typeof rows[number]>) => {
    setPrepared(null);
    setRows(current => current.map(row => row.path.id === id ? { ...row, ...patch } : row));
  };
  async function preview() {
    setBusy(true); setError(null);
    const input: WorkspaceSubprojectInput = {
      parent_id: parent.id, expected_revision: revision, parent_name: parentName.trim(),
      entries: selected.map(row => ({ workspace_id: row.path.id, name: row.name.trim(), aliases: row.aliases.split(/[,，]/).map(s => s.trim()).filter(Boolean), kind: row.kind })),
    };
    try { setPrepared({ input, preview: await agentApi.planWorkspaceSubprojects(input) }); }
    catch (reason) { setError(workspaceIssue(reason, '预览失败，请检查项目名称和目录。')); }
    finally { setBusy(false); }
  }
  async function apply() {
    if (!prepared) return;
    setBusy(true); setError(null);
    try { onApplied(await agentApi.applyWorkspaceSubprojects(prepared.input)); }
    catch (reason) { setPrepared(null); setError(workspaceIssue(reason, '未保存整理结果，请重新预览。')); }
    finally { setBusy(false); }
  }
  return <ModalSurface className="workspace-subprojects-dialog" titleId="workspace-subprojects-title" onClose={() => { if (!busy) onClose(); }}>
    <header className="workspace-registry-editor-head">
      <strong id="workspace-subprojects-title">整理子项目</strong>
      <button type="button" className="btn btn-icon" aria-label="关闭" disabled={busy} onClick={onClose}><X size={16} /></button>
    </header>
    <form className="workspace-subprojects-form" onSubmit={event => { event.preventDefault(); void (prepared ? apply() : preview()); }}>
      <label className="workspace-subprojects-parent">父项目别名<input required maxLength={40} value={parentName} disabled={busy} onChange={event => { setParentName(event.target.value); setPrepared(null); }} /></label>
      <p className="muted">选择目录并填写子项目别名，保存后可在父项目汇总查看。</p>
      <fieldset disabled={busy}>
        <legend className="sr-only">选择目录并设置子项目</legend>
        {rows.map(row => <div key={row.path.id} className="workspace-subprojects-row">
          <label className="workspace-subprojects-check"><input type="checkbox" checked={row.selected} onChange={event => update(row.path.id, { selected: event.target.checked })} />{row.aliases.split(/[,，]/)[0] || '目录'}</label>
          <code className="workspace-registry-path">{row.path.path}</code>
          {row.selected ? <>
            <label>子项目别名<input required maxLength={40} placeholder="中文别名" value={row.name} onChange={event => update(row.path.id, { name: event.target.value })} /></label>
            <label>其他名称<input value={row.aliases} onChange={event => update(row.path.id, { aliases: event.target.value })} /></label>
            <label>形态<select value={row.kind} onChange={event => update(row.path.id, { kind: event.target.value })}><option value="product">产品</option><option value="library">库</option><option value="aggregate">聚合仓库</option><option value="other">其他</option></select></label>
          </> : null}
        </div>)}
      </fieldset>
      {error ? <WorkspaceFeedback issue={error} /> : null}
      {prepared ? <section className="workspace-subprojects-preview" aria-label="整理预览">
        <strong>{projectDisplayName(prepared.preview.parent)} · {prepared.preview.changes.length} 个新子项目</strong>
        <ul>{prepared.preview.changes.map(change => <li key={change.workspace_id}><span>{projectDisplayName(change.project)}</span><code className="workspace-registry-path">{change.path}</code></li>)}</ul>
        <p>目录位置保持不变。会话按目录归属更新，保存后可恢复本次整理。</p>
      </section> : null}
      <footer className="actions-row">
        <span role="status">{busy ? '正在处理…' : `已选 ${selected.length} 个目录`}</span>
        <button type="button" className="btn" disabled={busy} onClick={onClose}>取消</button>
        <button type="submit" className="btn btn-primary" disabled={busy || !selected.length || !revision}>{prepared ? `保存 ${selected.length} 个子项目` : '预览整理'}</button>
      </footer>
    </form>
  </ModalSurface>;
}
