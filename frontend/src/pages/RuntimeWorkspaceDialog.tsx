import { useEffect, useState } from 'react';
import { BusyIndicator } from '../components/BusyIndicator';
import { agentApi, type MissionView, type RuntimeWorkspaceDiagnosis } from '../services/agentApi';
import { formatError } from '../types';
import { CollaborationDialog } from './CollaborationDialog';

export function RuntimeWorkspaceDialog({ mission, onClose, onResolved }: {
  mission: MissionView; onClose: () => void; onResolved: (mission: MissionView) => void;
}) {
  const [diagnosis, setDiagnosis] = useState<RuntimeWorkspaceDiagnosis | null>(null);
  const [checking, setChecking] = useState(true);
  const [busy, setBusy] = useState('');
  const [error, setError] = useState('');
  useEffect(() => {
    let cancelled = false;
    void agentApi.checkRuntimeWorkspace(mission.workspace.root)
      .then(value => { if (!cancelled) setDiagnosis(value); })
      .catch(cause => { if (!cancelled) setError(formatError(cause, '环境检查失败')); })
      .finally(() => { if (!cancelled) setChecking(false); });
    return () => { cancelled = true; };
  }, [mission.workspace.root]);

  async function resolve(action: 'full_access' | 'repair' | 'retry' | 'files_only') {
    if (busy || (!diagnosis && action !== 'full_access')) return;
    setBusy(action); setError('');
    try {
      const result = await agentApi.resolveRuntimeWorkspace({
        workspace: diagnosis?.workspace || mission.workspace.root, expected_fingerprint: diagnosis?.fingerprint || '', action,
        mission_id: mission.mission_id, expected_plan_sha256: mission.plan_sha256,
      });
      if (result.mission) onResolved(result.mission);
    } catch (cause) {
      let message = formatError(cause, '环境尚未恢复，请重新检查后选择');
      try { const structured = JSON.parse(String(cause)); if (typeof structured.message === 'string') message = structured.message; } catch { /* ordinary host error */ }
      setError(message);
      try { setDiagnosis(await agentApi.checkRuntimeWorkspace(mission.workspace.root)); } catch { /* keep the last observation visible */ }
    } finally { setBusy(''); }
  }

  return <CollaborationDialog titleId="runtime-workspace-title" onClose={() => { if (!busy) onClose(); }}>
    <section className="collaboration-create-dialog runtime-workspace-dialog">
      <header><div><span>任务已暂停 · 专家尚未开始</span><h2 id="runtime-workspace-title">处理命令环境</h2></div><button className="btn" disabled={Boolean(busy)} onClick={onClose}>稍后处理</button></header>
      <p>工作区沙箱未能启动。你可以直接授予本任务完全访问权限，继续执行。</p>
      <p className="task-evidence-path">{mission.workspace.root}</p>
      {busy || checking ? <p role="status"><BusyIndicator size={16} />{busy ? '正在应用执行权限，请稍候' : '正在检查隔离环境；你可以直接选择完全访问'}</p> : null}
      <section><h3>完全访问</h3><p>命令按你的系统账户权限执行，可访问工作区外文件。<br />无需手动修改目录权限，不自动提权为管理员。</p><button className="btn btn-primary" disabled={Boolean(busy) || mission.tasks.some(task => !['himind.builtin', 'personal.codex'].includes(task.spec.execution.executor) && !task.spec.execution.executor.startsWith('acp.'))} onClick={() => void resolve('full_access')}>授予完全访问并继续</button>{mission.tasks.some(task => !['himind.builtin', 'personal.codex'].includes(task.spec.execution.executor) && !task.spec.execution.executor.startsWith('acp.')) ? <small>完全访问支持 HiMind AI、Codex 和 ACP 执行器，请调整任务的执行器。</small> : null}</section>
      {diagnosis ? <>
        <details><summary>保留工作区隔离（高级）</summary><p>{diagnosis.message}</p>
        {diagnosis.repairable ? <section><h3>仅为此目录补充权限</h3><p>为当前用户增加此目录的“更改所有者”权限。<br />供 Windows 设置沙箱标签使用，授权不继承到子目录。<br />系统保留权限备份，目录所有者保持不变。</p><button className="btn btn-primary" disabled={Boolean(busy)} onClick={() => void resolve('repair')}>授权此目录并继续</button></section> : null}
        <section><h3>已在外部修复</h3><p>重新运行沙箱检查，通过后恢复原任务。</p><button className="btn" disabled={Boolean(busy)} onClick={() => void resolve('retry')}>重新检查并继续</button></section>
        </details>
        <section><h3>仅使用文件工具</h3><p>专家仍可读写计划允许的文件，命令工具将被禁用。<br />必需测试仍由宿主执行，验收标准保持有效。</p><button className="btn" disabled={Boolean(busy)} onClick={() => void resolve('files_only')}>仅用文件工具继续</button></section>
      </> : null}
      {error ? <p className="error" role="alert">{error}</p> : null}
      <footer><small>本次选择绑定到当前工作区与已确认计划。</small></footer>
    </section>
  </CollaborationDialog>;
}
