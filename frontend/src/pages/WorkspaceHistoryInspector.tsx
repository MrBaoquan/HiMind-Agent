import { useEffect, useRef, useState } from 'react';
import { agentApi, type SessionHistorySource, type WorkspaceHistoryHit, type WorkspaceHistoryInspection } from '../services/agentApi';

export function WorkspaceHistoryInspector({ projectId, includeChildren = false, projectNames = {}, sessionId, mode, source, onHit, onClose }: {
  projectId: string;
  includeChildren?: boolean;
  projectNames?: Record<string, string>;
  sessionId?: string;
  mode: 'text' | 'prompts' | 'files';
  source?: SessionHistorySource;
  onHit: (hit: WorkspaceHistoryHit) => void;
  onClose: () => void;
}) {
  const [query, setQuery] = useState('');
  const [run, setRun] = useState<{ query: string; version: number } | null>(mode === 'text' ? null : { query: '', version: 0 });
  const [hits, setHits] = useState<WorkspaceHistoryHit[]>([]);
  const [batch, setBatch] = useState<WorkspaceHistoryInspection | null>(null);
  const [issues, setIssues] = useState<string[]>([]);
  const [error, setError] = useState('');
  const [running, setRunning] = useState(false);
  const [cancelled, setCancelled] = useState(false);
  const stop = useRef<() => void>(() => {});
  const [active, setActive] = useState(-1);
  const resultList = useRef<HTMLOListElement>(null);
  const label = mode === 'text' ? '正文搜索' : mode === 'prompts' ? '提问导航' : '相关文件';

  useEffect(() => {
    if (!run) return;
    let alive = true;
    let token: string | undefined;
    setHits([]); setBatch(null); setIssues([]); setError(''); setRunning(true); setCancelled(false); setActive(-1);
    const cancel = () => {
      alive = false;
      setRunning(false); setCancelled(true);
      if (token) void agentApi.cancelWorkspaceSessionInspection(projectId, token).catch(() => {});
    };
    stop.current = cancel;
    void (async () => {
      try {
        token = await agentApi.startWorkspaceSessionInspection(projectId, sessionId, run.query, mode, source, includeChildren);
        if (!alive) { await agentApi.cancelWorkspaceSessionInspection(projectId, token); return; }
        for (;;) {
          const result = await agentApi.stepWorkspaceSessionInspection(projectId, token);
          if (!alive) return;
          setHits(items => [...items, ...result.hits]);
          setBatch(result);
          setIssues(items => [...new Set([...items, ...result.issues])]);
          if (result.done) { setRunning(false); token = undefined; return; }
        }
      } catch (reason) {
        if (alive) { setError(String(reason).replace(/^Error:\s*/, '')); setRunning(false); }
        if (token) { void agentApi.cancelWorkspaceSessionInspection(projectId, token).catch(() => {}); token = undefined; }
      }
    })();
    return () => { alive = false; if (token) void agentApi.cancelWorkspaceSessionInspection(projectId, token).catch(() => {}); };
  }, [projectId, includeChildren, sessionId, mode, source, run]);

  const move = (direction: number) => {
    if (!hits.length) return;
    const next = Math.max(0, Math.min(hits.length - 1, active + direction));
    setActive(next);
    const button = resultList.current?.querySelectorAll<HTMLButtonElement>('li > button')[next];
    button?.focus({ preventScroll: true });
    const list = resultList.current;
    if (list && button) {
      const top = button.getBoundingClientRect().top - list.getBoundingClientRect().top + list.scrollTop;
      list.scrollTo({ top: Math.max(0, top - list.clientHeight / 3) });
    }
  };

  return <section className="workspace-history-inspector" aria-label={label} onKeyDown={event => {
    if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); onClose(); }
    if (event.altKey && (event.key === 'ArrowDown' || event.key === 'ArrowUp')) { event.preventDefault(); move(event.key === 'ArrowDown' ? 1 : -1); }
  }}>
    <header className="workspace-history-inspector-head">
      <strong tabIndex={-1}>{label}</strong>
      <button type="button" className="btn" onClick={onClose}>关闭{label}</button>
    </header>
    {mode !== 'prompts' ? <form className="workspace-history-query" onSubmit={event => { event.preventDefault(); if (query.trim() || mode === 'files') setRun(previous => ({ query: query.trim(), version: (previous?.version ?? 0) + 1 })); }}>
      <label className="sr-only" htmlFor="workspace-history-body-query">{mode === 'text' ? '搜索当前项目正文' : '按路径筛选相关文件'}</label>
      <input id="workspace-history-body-query" type="search" maxLength={200} value={query} onChange={event => setQuery(event.target.value)} placeholder={mode === 'text' ? '搜索当前项目正文' : '按路径筛选相关文件'} autoFocus />
      <button type="submit" className="btn" disabled={mode === 'text' && !query.trim()}>搜索</button>
    </form> : null}
    <div className="workspace-history-inspector-status" role="status">
      <span>{running ? '扫描中' : cancelled ? '已取消' : batch?.done ? '扫描结束' : '输入关键词开始搜索'} · {hits.length} 项{batch ? ` · 已读 ${batch.scanned_sessions}/${batch.total_sessions} 个会话，${batch.scanned_pages} 页` : ''}</span>
      {running ? <button type="button" className="btn" onClick={() => stop.current()}>取消扫描</button> : null}
    </div>
    <p className="workspace-history-inspector-hint">{mode === 'text' ? `范围：${includeChildren ? '当前项目及子项目' : '当前项目'}的可读取文字与文件操作记录。` : mode === 'files' ? '范围：Claude/Codex 历史工具请求，执行结果未核实。' : '按用户角色筛选，排除已识别的系统与命令记录。'} Alt+↑ / ↓ 选择条目。</p>
    {batch?.limited ? <p className="workspace-history-inspector-warning" role="status">已达扫描上限，结果不完整。请缩小来源或关键词范围。</p> : null}
    {batch?.sources.some(source => source.availability !== 'available') || issues.length ? <details className="workspace-history-inspector-warning"><summary>部分来源或会话未读取</summary><ul>
      {batch?.sources.filter(source => source.availability !== 'available').map(source => <li key={source.source}>{source.source}：{source.error_code || source.availability}</li>)}
      {issues.map(issue => <li key={issue}>{issue}</li>)}
    </ul></details> : null}
    {error ? <div role="alert" className="workspace-history-inspector-warning"><p>读取失败，可重新扫描。</p><code>{error}</code><button type="button" className="btn" onClick={() => setRun(previous => ({ query: previous?.query ?? '', version: (previous?.version ?? 0) + 1 }))}>重试扫描</button></div> : null}
    <ol className="workspace-history-inspector-list" ref={resultList} aria-label={`${label}结果`} tabIndex={0}>
      {hits.map((hit, index) => <li key={`${hit.session.id}:${hit.message_id}:${index}`}><button type="button" className={active === index ? 'selected' : ''} onFocus={() => setActive(index)} onClick={() => onHit(hit)}>
        <strong>{hit.paths.length ? hit.paths.join(' · ') : hit.excerpt || '无文字消息'}</strong>
        <span>{hit.session.title} · {hit.session.project_id ? projectNames[hit.session.project_id] || hit.session.project_name : hit.session.project_name} · {hit.session.source}</span>
        <span>定位来源消息</span>
      </button></li>)}
      {!running && batch?.done && hits.length === 0 ? <li className="workspace-session-history-empty">没有匹配的记录</li> : null}
    </ol>
  </section>;
}
