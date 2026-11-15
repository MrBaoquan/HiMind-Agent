import { useEffect, useRef, useState } from 'react';
import { agentApi, type WorkspaceSessionSummary } from '../services/agentApi';

export function WorkspaceHistoryActions({ projectId, session }: { projectId: string; session: WorkspaceSessionSummary }) {
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState('');
  const [error, setError] = useState('');
  const generation = useRef(0);
  useEffect(() => { generation.current += 1; setNotice(''); setError(''); setBusy(false); return () => { generation.current += 1; }; }, [projectId, session.id]);
  if (session.source !== 'claude_code' && session.source !== 'codex') return null;
  const run = async (copy: boolean) => {
    if (busy) return;
    const id = generation.current;
    setBusy(true); setError(''); setNotice('');
    try {
      if (copy) {
        const plan = await agentApi.getWorkspaceSessionResumePlan(projectId, session.id);
        if (id !== generation.current) return;
        await navigator.clipboard.writeText(plan.command);
        setNotice(plan.available ? '已复制 PowerShell 恢复命令' : '已复制命令；请先安装原客户端');
      } else {
        await agentApi.resumeWorkspaceSession(projectId, session.id);
        if (id === generation.current) setNotice('已打开终端；请在原客户端继续操作');
      }
    } catch (reason) {
      if (id === generation.current) {
        const code = String(reason).replace(/^Error:\s*/, '');
        const messages: Record<string, string> = {
          SESSION_CLIENT_NOT_FOUND: '未检测到原客户端，请安装后重试。',
          SESSION_ALREADY_LAUNCHING: '该会话刚刚启动，请查看终端。',
          SESSION_DIRECTORY_UNAVAILABLE: '原工作目录不存在或无法访问。',
          SESSION_DIRECTORY_CHANGED: '目录归属已变化，请刷新列表。',
          SESSION_NOT_FOUND: '会话已变化，请刷新列表。',
          SESSION_RESUME_ID_INVALID: '该会话的标识不支持安全恢复。',
        };
        setError(messages[code] || `操作失败：${code}`);
      }
    } finally { if (id === generation.current) setBusy(false); }
  };
  return <div className="workspace-history-actions">
    <button type="button" className="btn" disabled={busy || !session.capabilities.can_resume} title={!session.capabilities.can_resume ? '未检测到可用客户端或会话标识不支持恢复' : '在原客户端中继续指定会话'} onClick={() => void run(false)}>继续会话</button>
    <button type="button" className="btn" disabled={busy} onClick={() => void run(true)}>复制恢复命令</button>
    {!session.capabilities.can_resume ? <span>原客户端暂不可用</span> : null}
    {notice ? <span role="status">{notice}</span> : null}
    {error ? <span role="alert">{error}</span> : null}
  </div>;
}
