import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { AlertCircle, Clock3, Eye, FolderOpen, History, MessageSquareText, RefreshCw, Search, X } from 'lucide-react';
import { agentApi, type SessionHistorySource, type WorkspaceHistoryHit, type WorkspaceSessionHistory, type WorkspaceSessionHistorySort, type WorkspaceSessionSummary, type WorkspaceSessionTranscriptPage } from '../services/agentApi';
import './WorkspaceSessionHistoryPanel.css';
import { WorkspaceFeedback, workspaceIssue } from './WorkspaceFeedback';
import { WorkspaceHistoryInspector } from './WorkspaceHistoryInspector';
import { WorkspaceHistoryActions } from './WorkspaceHistoryActions';

type Props = {
  projectId: string;
  loadHistory?: (projectId: string, cursor?: string, query?: string, source?: SessionHistorySource, sort?: WorkspaceSessionHistorySort) => Promise<WorkspaceSessionHistory>;
  loadTranscript?: (projectId: string, sessionId: string, cursor?: string) => Promise<WorkspaceSessionTranscriptPage>;
};

type PageState = {
  key: string;
  cursor?: string;
  previous: Array<string | undefined>;
};

type TranscriptNavigation = {
  cursor?: string;
  previous: Array<string | undefined>;
};

function formatTime(value?: string | null): string {
  if (!value) return '时间未知';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return '时间未知';
  return new Intl.DateTimeFormat('zh-CN', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  }).format(date);
}

function sourceLabel(source: string): string {
  switch (source) {
    case 'dsh': return 'HiMind AI';
    case 'acp': return 'ACP';
    case 'claude_code': return 'Claude Code';
    case 'codex': return 'Codex';
    case 'open_code':
    case 'opencode': return 'OpenCode';
    default: return '未知来源';
  }
}

function sourceStatusText(source: WorkspaceSessionHistory['sources'][number]): string {
  const label = sourceLabel(source.source);
  if (source.availability === 'available') return `${label} · ${source.session_count}`;
  if (source.availability === 'partial') return `${label} · 部分不可用`;
  if (source.availability === 'unavailable') return `${label} · 未检测到`;
  return `${label} · 读取失败`;
}

function sourceRecoveryHint(source: WorkspaceSessionHistory['sources'][number]): string {
  if (source.error_code === 'runtime_not_running') return '请启动本地客户端后重新读取。';
  if (source.availability === 'unavailable') return '请确认本机已安装并使用该客户端。';
  if (source.availability === 'partial') return '可浏览已读取的会话；请检查本地会话文件后重试。';
  return '请确认本地会话文件可读取后重试。';
}

export function WorkspaceSessionHistoryPanel({
  projectId,
  loadHistory = agentApi.listWorkspaceSessionHistory,
  loadTranscript = agentApi.getWorkspaceSessionTranscriptPage,
}: Props) {
  const [search, setSearch] = useState('');
  const [query, setQuery] = useState('');
  const [sourceFilter, setSourceFilter] = useState<SessionHistorySource | ''>('');
  const [sort, setSort] = useState<WorkspaceSessionHistorySort>('recent');
  const [page, setPage] = useState<PageState>({ key: '', previous: [] });
  const [snapshot, setSnapshot] = useState<WorkspaceSessionHistory | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [refreshToken, setRefreshToken] = useState(0);
  const requestId = useRef(0);
  const transcriptRequestId = useRef(0);
  const [selectedSession, setSelectedSession] = useState<WorkspaceSessionSummary | null>(null);
  const [transcriptPage, setTranscriptPage] = useState<WorkspaceSessionTranscriptPage | null>(null);
  const [transcriptNavigation, setTranscriptNavigation] = useState<TranscriptNavigation>({ previous: [] });
  const [transcriptLoading, setTranscriptLoading] = useState(false);
  const [transcriptError, setTranscriptError] = useState('');
  const previewTitle = useRef<HTMLHeadingElement>(null);
  const historyTitle = useRef<HTMLHeadingElement>(null);
  const previewTrigger = useRef<HTMLElement | null>(null);
  const savedScroll = useRef<Array<{ element: Element; top: number; left: number }>>([]);
  const restoreList = useRef(false);
  const retryTranscript = useRef<() => void>(() => {});
  const listScroll = useRef<HTMLOListElement>(null);
  const messageScroll = useRef<HTMLOListElement>(null);
  const loadedPageKey = useRef('');
  const [inspection, setInspection] = useState<{ mode: 'text' | 'prompts' | 'files'; sessionId?: string; visible: boolean } | null>(null);
  const [targetMessage, setTargetMessage] = useState('');
  const [copyNotice, setCopyNotice] = useState('');
  const [locationChanged, setLocationChanged] = useState(false);
  const inspectionVisible = Boolean(inspection?.visible);
  const inspectorTrigger = useRef<HTMLElement | null>(null);
  const inspectorSlot = useRef<HTMLDivElement>(null);
  const showInspector = (mode: 'text' | 'prompts' | 'files', sessionId?: string) => {
    inspectorTrigger.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setInspection({ mode, sessionId, visible: true });
    requestAnimationFrame(() => (inspectorSlot.current?.querySelector<HTMLElement>('input') ?? inspectorSlot.current?.querySelector<HTMLElement>('.workspace-history-inspector-head strong'))?.focus({ preventScroll: true }));
  };
  const closeInspector = () => {
    setInspection(null);
    requestAnimationFrame(() => (inspectorTrigger.current?.isConnected ? inspectorTrigger.current : selectedSession ? previewTitle.current : historyTitle.current)?.focus({ preventScroll: true }));
  };

  useLayoutEffect(() => {
    if (!targetMessage || inspectionVisible || transcriptLoading) return;
    const list = messageScroll.current;
    const message = list && [...list.querySelectorAll<HTMLElement>('[data-message-id]')].find(element => element.dataset.messageId === targetMessage);
    if (list && message) {
      list.scrollTo({ top: Math.max(0, message.getBoundingClientRect().top - list.getBoundingClientRect().top + list.scrollTop - 12) });
      message.focus({ preventScroll: true });
    }
  }, [targetMessage, transcriptPage, transcriptLoading, inspectionVisible]);

  useEffect(() => {
    return () => { requestId.current += 1; transcriptRequestId.current += 1; };
  }, [projectId]);

  useLayoutEffect(() => {
    if (selectedSession) {
      // Embedded workspace panes already place the preview at the top of the viewport.
      // scrollIntoView on them also moves the shell and destroys the user's position.
      if (!previewTitle.current?.closest('.workspace-registry-history-pane')) previewTitle.current?.scrollIntoView({ block: 'start', behavior: 'instant' });
      previewTitle.current?.focus({ preventScroll: true });
    } else if (restoreList.current) {
      restoreList.current = false;
      const trigger = previewTrigger.current?.isConnected ? previewTrigger.current : historyTitle.current;
      trigger?.focus({ preventScroll: true });
      for (const entry of savedScroll.current) entry.element.scrollTo(entry.left, entry.top);
      previewTrigger.current = null;
      savedScroll.current = [];
    }
  }, [selectedSession?.id]);

  const pageKey = JSON.stringify([projectId, query, sourceFilter, sort]);
  const cursor = page.key === pageKey ? page.cursor : undefined;
  const previous = page.key === pageKey ? page.previous : [];

  useEffect(() => {
    const timer = window.setTimeout(() => setQuery(search.trim()), 250);
    return () => window.clearTimeout(timer);
  }, [search]);

  useEffect(() => {
    const id = ++requestId.current;
    const resultKey = JSON.stringify([pageKey, cursor]);
    let active = true;
    setLoading(true);
    setError('');
    void loadHistory(projectId, cursor, query, sourceFilter || undefined, sort)
      .then(result => {
        if (active && id === requestId.current) {
          setSnapshot(result);
          if (loadedPageKey.current !== resultKey) listScroll.current?.scrollTo({ top: 0 });
          loadedPageKey.current = resultKey;
        }
      })
      .catch(reason => {
        if (active && id === requestId.current) {
          setError(String(reason).replace(/^Error:\s*/, ''));
          setSnapshot(null);
        }
      })
      .finally(() => {
        if (active && id === requestId.current) setLoading(false);
      });
    return () => { active = false; };
  }, [projectId, cursor, query, sourceFilter, sort, refreshToken, loadHistory]);

  const sourceStatuses = snapshot?.sources ?? [];
  const pageStart = snapshot?.total ? (Number(cursor ?? 0) + 1) : 0;
  const pageEnd = Math.min(Number(cursor ?? 0) + (snapshot?.sessions.length ?? 0), snapshot?.total ?? 0);

  const openTranscript = async (session: WorkspaceSessionSummary, hit?: WorkspaceHistoryHit) => {
    if (!session.capabilities.can_preview) return;
    if (!selectedSession && !previewTrigger.current) {
      previewTrigger.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      savedScroll.current = [];
      for (let parent = previewTrigger.current?.parentElement; parent; parent = parent.parentElement) {
        if (parent.scrollHeight > parent.clientHeight) savedScroll.current.push({ element: parent, top: parent.scrollTop, left: parent.scrollLeft });
      }
      const scrolling = document.scrollingElement;
      if (scrolling && !savedScroll.current.some(entry => entry.element === scrolling)) savedScroll.current.push({ element: scrolling, top: scrolling.scrollTop, left: scrolling.scrollLeft });
    }
    retryTranscript.current = () => { void openTranscript(session, hit); };
    const id = ++transcriptRequestId.current;
    setSelectedSession(session);
    setTranscriptPage(null);
    setTranscriptNavigation(hit ? { cursor: hit.cursor ?? undefined, previous: hit.previous.map(value => value ?? undefined) } : { previous: [] });
    setTargetMessage(hit?.message_id ?? '');
    setCopyNotice('');
    setLocationChanged(false);
    setInspection(value => value ? { ...value, visible: false } : null);
    setTranscriptError('');
    setTranscriptLoading(true);
    try {
      const result = await loadTranscript(projectId, session.id, hit?.cursor ?? undefined);
      if (id === transcriptRequestId.current) {
        setTranscriptPage(result);
        if (hit && !result.messages.some(message => message.id === hit.message_id)) { setTranscriptError('来源记录已变化，请重新搜索定位。'); setLocationChanged(true); }
      }
    } catch (reason) {
      if (id === transcriptRequestId.current) setTranscriptError(String(reason).replace(/^Error:\s*/, ''));
    } finally {
      if (id === transcriptRequestId.current) setTranscriptLoading(false);
    }
  };

  const closeTranscript = () => {
    restoreList.current = true;
    transcriptRequestId.current += 1;
    setSelectedSession(null);
    setTranscriptPage(null);
    setTranscriptNavigation({ previous: [] });
    setTranscriptError('');
    setTranscriptLoading(false);
    setTargetMessage('');
    setCopyNotice('');
    setLocationChanged(false);
    setInspection(value => value?.sessionId ? null : value ? { ...value, visible: true } : null);
  };

  const navigateTranscript = async (navigation: TranscriptNavigation) => {
    if (!selectedSession || transcriptLoading) return;
    retryTranscript.current = () => { void navigateTranscript(navigation); };
    const id = ++transcriptRequestId.current;
    setTranscriptLoading(true);
    setTranscriptError('');
    try {
      const result = await loadTranscript(projectId, selectedSession.id, navigation.cursor);
      if (id === transcriptRequestId.current) {
        setTranscriptPage(result);
        setTranscriptNavigation(navigation);
        setTargetMessage('');
        messageScroll.current?.scrollTo({ top: 0 });
      }
    } catch (reason) {
      if (id === transcriptRequestId.current) setTranscriptError(String(reason).replace(/^Error:\s*/, ''));
    } finally {
      if (id === transcriptRequestId.current) setTranscriptLoading(false);
    }
  };

  const loadOlderMessages = () => {
    if (!transcriptPage?.next_cursor) return;
    void navigateTranscript({
      cursor: transcriptPage.next_cursor,
      previous: [...transcriptNavigation.previous, transcriptNavigation.cursor],
    });
  };

  const loadNewerMessages = () => {
    const previous = transcriptNavigation.previous;
    if (previous.length === 0) return;
    void navigateTranscript({
      cursor: previous[previous.length - 1],
      previous: previous.slice(0, -1),
    });
  };

  return (
    <section className="workspace-session-history" aria-labelledby="workspace-session-history-title" aria-busy={loading}>
      <header className="workspace-session-history-head">
        <div className="workspace-session-history-title">
          <History size={17} aria-hidden="true" />
          <h4 ref={historyTitle} tabIndex={-1} id="workspace-session-history-title">会话历史</h4>
          {snapshot ? <span className="workspace-session-history-total">{snapshot.total}</span> : null}
          {loading && snapshot ? <span className="workspace-session-history-total" role="status">更新中…</span> : null}
          <button type="button" className="btn btn-icon" hidden={Boolean(selectedSession) || inspectionVisible} title="搜索正文" aria-label="搜索正文" onClick={() => showInspector('text')}><Search size={14} aria-hidden="true" /></button>
          <button type="button" className="btn btn-icon" hidden={Boolean(selectedSession) || inspectionVisible} title="项目相关文件" aria-label="项目相关文件" onClick={() => showInspector('files')}><FolderOpen size={14} aria-hidden="true" /></button>
        </div>
        <div className="workspace-session-history-tools" hidden={Boolean(selectedSession) || inspectionVisible}>
          <label className="workspace-session-history-search">
            <Search size={14} aria-hidden="true" />
            <span className="sr-only">搜索会话标题或目录</span>
            <input
              type="search"
              value={search}
              onChange={event => setSearch(event.target.value)}
              placeholder="搜索会话标题或目录"
              aria-label="搜索会话标题或目录"
            />
          </label>
          <label className="workspace-session-history-filter">
            <span className="sr-only">按来源筛选</span>
            <select
              value={sourceFilter}
              onChange={event => setSourceFilter(event.target.value as SessionHistorySource | '')}
              aria-label="按来源筛选"
            >
              <option value="">所有来源</option>
              {sourceStatuses.map(source => (
                <option key={source.source} value={source.source}>{sourceLabel(source.source)}</option>
              ))}
            </select>
          </label>
          <label className="workspace-session-history-filter">
            <span className="sr-only">排序会话</span>
            <select value={sort} onChange={event => setSort(event.target.value as WorkspaceSessionHistorySort)} aria-label="排序会话">
              <option value="recent">最近更新</option>
              <option value="oldest">最早更新</option>
              <option value="title">标题升序</option>
            </select>
          </label>
          <button
            type="button"
            className="btn btn-icon"
            title="刷新会话历史"
            aria-label="刷新会话历史"
            disabled={loading}
            onClick={() => setRefreshToken(value => value + 1)}
          >
            <RefreshCw size={14} />
          </button>
        </div>
      </header>

      {inspection ? <div ref={inspectorSlot} className="workspace-history-inspector-slot" hidden={!inspectionVisible}>
        <WorkspaceHistoryInspector key={`${projectId}:${inspection.sessionId ?? ''}:${inspection.mode}:${sourceFilter}`} projectId={projectId} sessionId={inspection.sessionId} source={sourceFilter || undefined} mode={inspection.mode} onHit={hit => void openTranscript(hit.session, hit)} onClose={closeInspector} />
      </div> : null}

      <details className="workspace-session-history-source-details" hidden={Boolean(selectedSession) || inspectionVisible}>
        <summary>来源状态{sourceStatuses.length ? ` · ${sourceStatuses.filter(source => source.availability === 'available').length} 个可用` : ''}{sourceStatuses.some(source => source.availability !== 'available') ? ' · 有来源未就绪' : ''}</summary>
        {!selectedSession && sourceStatuses.length > 0 ? (
          <div className="workspace-session-history-sources" role="status" aria-label="会话来源状态">
            {sourceStatuses.map(source => (
              <span key={source.source} className={`workspace-session-history-source-state ${source.availability}`}>
                {source.availability !== 'available' ? <AlertCircle size={13} aria-hidden="true" /> : null}
                {sourceStatusText(source)}
              </span>
            ))}
          </div>
        ) : null}
        {!selectedSession && sourceStatuses.some(source => source.availability !== 'available') ? <details className="workspace-session-history-recovery">
          <summary>来源状态与恢复建议</summary>
          <ul>{sourceStatuses.filter(source => source.availability !== 'available').map(source => <li key={source.source}>
            <strong>{sourceLabel(source.source)}</strong>：{sourceRecoveryHint(source)}
          </li>)}</ul>
          <p>处理后，使用上方刷新按钮重新读取。</p>
          <details><summary>诊断详情</summary><pre tabIndex={0}>{sourceStatuses.filter(source => source.error_code).map(source => `${sourceLabel(source.source)}: ${source.error_code}`).join('\n') || '无诊断码'}</pre></details>
        </details> : null}
      </details>
      {error ? <WorkspaceFeedback issue={workspaceIssue(error, '会话列表读取失败，请重试。')} disabled={loading} retryLabel="刷新列表" onRetry={() => { setPage({ key: pageKey, previous: [] }); setRefreshToken(value => value + 1); }} /> : null}

      {loading && !snapshot ? (
        <div className="workspace-session-history-loading" role="status">读取会话历史…</div>
      ) : null}

      {snapshot || selectedSession ? (
        <>
          {snapshot && snapshot.sessions.length > 0 ? <div className="workspace-session-history-results" hidden={Boolean(selectedSession) || inspectionVisible}>
            <ol ref={listScroll} className="workspace-session-history-list" tabIndex={0} aria-label="会话列表">
              {snapshot.sessions.map(session => (
                <li key={session.id}>
                  <article className={`workspace-session-history-row${selectedSession?.id === session.id ? ' selected' : ''}`}>
                    <div className="workspace-session-history-row-main">
                      <strong title={session.title}>{session.title || '未命名会话'}</strong>
                      <code title={session.working_directory}><FolderOpen size={13} aria-hidden="true" />{session.working_directory || '工作目录未知'}</code>
                    </div>
                    <div className="workspace-session-history-row-meta">
                      <span><Clock3 size={13} aria-hidden="true" />{formatTime(session.updated_at)}</span>
                      {session.message_count != null ? <span><MessageSquareText size={13} aria-hidden="true" />{session.message_count}</span> : null}
                      <span className="workspace-session-history-source">{sourceLabel(session.source)}</span>
                    </div>
                    <div className="workspace-session-history-row-actions">
                      {session.capabilities.can_preview ? (
                        <button type="button" className="btn" disabled={loading} onClick={() => void openTranscript(session)} aria-label={`预览会话：${session.title}`}>
                          <Eye size={13} aria-hidden="true" />预览
                        </button>
                      ) : <span>仅列表</span>}
                    </div>
                  </article>
                </li>
              ))}
            </ol>
            <footer className="workspace-session-history-pagination">
              <span>{pageStart}–{pageEnd} / {snapshot.total}</span>
              <div>
                <button
                  type="button"
                  className="btn"
                  disabled={loading || previous.length === 0}
                  onClick={() => {
                    const nextPrevious = previous.slice(0, -1);
                    setPage({ key: pageKey, cursor: previous[previous.length - 1], previous: nextPrevious });
                  }}
                >上一页</button>
                <button
                  type="button"
                  className="btn"
                  disabled={loading || !snapshot.next_cursor}
                  onClick={() => setPage({ key: pageKey, cursor: snapshot.next_cursor ?? undefined, previous: [...previous, cursor] })}
                >下一页</button>
              </div>
            </footer>
          </div> : null}
          {selectedSession ? (
            <section className="workspace-session-history-preview" hidden={inspectionVisible} aria-label="会话内容预览" aria-busy={transcriptLoading} onKeyDown={event => { if (event.key === 'Escape') { event.stopPropagation(); closeTranscript(); } }}>
              <header className="workspace-session-history-preview-head">
                <div>
                  <MessageSquareText size={14} aria-hidden="true" />
                  <h5 ref={previewTitle} tabIndex={-1} title={selectedSession.title}>{selectedSession.title}</h5>
                </div>
                <button type="button" className="btn" title="关闭预览" aria-label="关闭预览" onClick={closeTranscript}>
                  <X size={14} aria-hidden="true" /><span>返回列表</span>
                </button>
              </header>
              <div className="workspace-history-preview-tools">
                <WorkspaceHistoryActions projectId={projectId} session={selectedSession} />
                <div className="workspace-history-entry">
                  <button type="button" className="btn" onClick={() => showInspector('prompts', selectedSession.id)}>提问导航</button>
                  {selectedSession.source === 'claude_code' || selectedSession.source === 'codex' ? <button type="button" className="btn" onClick={() => showInspector('files', selectedSession.id)}>相关文件</button> : null}
                </div>
                {copyNotice ? <span role="status">{copyNotice}</span> : null}
              </div>
              {transcriptLoading && !transcriptPage ? <div className="workspace-session-history-loading" role="status">读取消息页…</div> : null}
              {transcriptError ? <WorkspaceFeedback issue={workspaceIssue(transcriptError, locationChanged ? '来源记录已变化，请返回结果重新扫描。' : '消息读取失败，请重试。')} disabled={transcriptLoading} retryLabel={locationChanged ? '返回结果' : undefined} onRetry={() => { if (locationChanged && inspection) showInspector(inspection.mode, inspection.sessionId); else retryTranscript.current(); }} /> : null}
              {transcriptPage && transcriptPage.messages.length > 0 ? (
                <ol ref={messageScroll} className="workspace-session-history-messages" tabIndex={0} aria-label="会话消息">
                  {transcriptPage.messages.map(message => (
                    <li key={`${transcriptPage.session_id}:${message.id}`} data-message-id={message.id} tabIndex={-1} className={`workspace-session-history-message ${message.role}${message.id === targetMessage ? ' located' : ''}`}>
                      <div>
                        <strong>{message.role === 'assistant' ? sourceLabel(selectedSession.source) : message.role === 'context' ? '上下文' : '你'}</strong>
                        {message.timestamp ? <time>{formatTime(message.timestamp)}</time> : null}
                        <button type="button" className="btn" aria-label={`复制消息 ${message.id}`} onClick={() => void navigator.clipboard.writeText(message.content).then(() => setCopyNotice('已复制消息')).catch(() => setCopyNotice('复制失败，请选择文字后复制'))}>复制</button>
                      </div>
                      <pre>{message.content}</pre>
                      {message.files?.map((file, index) => <details className="workspace-history-file" key={`${file.path}:${index}`}>
                        <summary>{file.operation} · {file.path}</summary>
                        <p>{file.kind === 'fragment' ? '记录片段对照' : file.kind === 'patch' ? '历史补丁' : file.kind === 'reference' ? '文件读取记录' : '记录中的写入内容'} · 工具请求，执行结果未核实</p>
                        {file.before != null ? <div><strong>原片段</strong><pre className="workspace-history-diff-before">{file.before}</pre></div> : null}
                        {file.after != null ? <div><strong>{file.kind === 'fragment' ? '新片段' : '内容'}</strong><pre className="workspace-history-diff-after">{file.after}</pre></div> : null}
                        {file.patch != null ? <pre>{file.patch}</pre> : null}
                        {file.truncated ? <p>内容超过展示上限，已截断。</p> : null}
                      </details>)}
                    </li>
                  ))}
                </ol>
              ) : null}
              {transcriptPage && transcriptPage.messages.length === 0 ? (
                <div className="workspace-session-history-empty">此会话没有可预览的文字消息</div>
              ) : null}
              {transcriptPage ? (
                <footer className="workspace-session-history-pagination">
                  {transcriptPage.through_seq !== null ? <span>序列边界 {transcriptPage.through_seq}</span> : null}
                  <div>
                    <button type="button" className="btn" disabled={transcriptLoading || transcriptNavigation.previous.length === 0} onClick={loadNewerMessages}>较新消息</button>
                    <button type="button" className="btn" disabled={transcriptLoading || !transcriptPage.next_cursor} onClick={loadOlderMessages}>
                      更早消息
                    </button>
                  </div>
                </footer>
              ) : null}
            </section>
          ) : null}
        </>
      ) : null}

      {snapshot && snapshot.sessions.length === 0 && !selectedSession && !inspectionVisible ? (
        <div className="workspace-session-history-empty">
          <History size={17} aria-hidden="true" />
          <span>{loading ? '正在更新…' : query || sourceFilter ? '没有匹配的会话' : '暂无已关联的会话'}</span>
        </div>
      ) : null}
    </section>
  );
}
