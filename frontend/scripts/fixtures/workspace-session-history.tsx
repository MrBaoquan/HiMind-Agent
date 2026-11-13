import { useCallback, useRef } from 'react';
import { createRoot } from 'react-dom/client';
import '../../styles.css';
import { WorkspaceSessionHistoryPanel } from '../../src/pages/WorkspaceSessionHistoryPanel';
import { agentApi, type SessionHistorySource, type WorkspaceHistoryHit, type WorkspaceSessionHistory, type WorkspaceSessionHistorySort, type WorkspaceSessionTranscriptPage } from '../../src/services/agentApi';

const historySources: SessionHistorySource[] = ['dsh', 'acp', 'claude_code', 'codex', 'opencode'];

const sessions = Array.from({ length: 55 }, (_, index) => {
  const number = String(index + 1).padStart(3, '0');
  const source = historySources[index % historySources.length];
  return {
    id: `${source}:synthetic-${number}`,
    source,
    native_id: `synthetic-${number}`,
    project_id: 'fixture-project',
    project_name: 'Fixture Project',
    working_directory: `F:/fixture/project/module-${number}`,
    title: `Synthetic session ${number}`,
    updated_at: new Date(Date.UTC(2026, 10, 11, 12, index)).toISOString(),
    message_count: 12 + index,
    capabilities: { can_preview: true, can_open_original: false, can_resume: source === 'claude_code' || source === 'codex' },
  };
});

const params = new URLSearchParams(window.location.search);
const jobs = new Map<string, { project: string; session?: string; mode: string; query: string; page: number }>();
let tokenNumber = 0;
agentApi.getWorkspaceSessionResumePlan = async (_project, sessionId) => ({ session_id: sessionId, client: 'Codex', working_directory: 'F:/fixture/project', command: "Set-Location -LiteralPath 'F:/fixture/project'; & 'codex' 'resume' 'synthetic-054'", available: true });
agentApi.resumeWorkspaceSession = async (_project, sessionId) => {
  window.dispatchEvent(new CustomEvent('history-fixture-action', { detail: { kind: 'resume', sessionId } }));
  if (params.has('resume-error')) throw new Error('SESSION_DIRECTORY_CHANGED');
};
agentApi.startWorkspaceSessionInspection = async (project, session, query, mode) => {
  const token = `inspection-${++tokenNumber}`;
  jobs.set(token, { project, session, query, mode, page: 0 });
  window.dispatchEvent(new CustomEvent('history-fixture-action', { detail: { kind: 'start', project, session, mode, query, token } }));
  return token;
};
agentApi.cancelWorkspaceSessionInspection = async (_project, token) => {
  jobs.delete(token);
  window.dispatchEvent(new CustomEvent('history-fixture-action', { detail: { kind: 'cancel', token } }));
};
agentApi.stepWorkspaceSessionInspection = async (_project, token) => {
  const job = jobs.get(token);
  if (!job) throw new Error('SESSION_SEARCH_EXPIRED');
  await new Promise(resolve => window.setTimeout(resolve, job.query === 'slow' ? 800 : 30));
  if (job.query === 'fixture-error') throw new Error('synthetic_search_failure');
  const session = sessions.find(session => session.id === job.session) ?? sessions[53];
  const hits: WorkspaceHistoryHit[] = job.query === 'no-such-body' ? [] : [{ session, message_id: job.mode === 'files' ? '2' : '0', cursor: job.mode === 'files' ? '50' : '0', previous: job.mode === 'files' ? [null] : [null, '50'], excerpt: `Synthetic message 000 · ${job.query}`, paths: job.mode === 'files' ? ['src/synthetic-file.ts'] : [] }];
  job.page += 1;
  return { token, hits: job.page === 1 ? hits : [], sources: [], scanned_pages: job.page, scanned_sessions: job.page === 1 ? 0 : 1, total_sessions: 1, done: job.query !== 'slow' && job.page >= 2, limited: job.query === 'limited', cancelled: false, issues: job.query === 'partial' ? ['codex: synthetic_unavailable'] : [] };
};

function App() {
  const latestTranscriptReads = useRef(0);
  const loadHistory = useCallback(async (
    _projectId: string,
    cursor?: string,
    query?: string,
    source?: 'dsh' | 'acp' | 'claude_code' | 'codex' | 'opencode',
    sort?: WorkspaceSessionHistorySort,
  ): Promise<WorkspaceSessionHistory> => {
    if (query === 'fixture-error') throw new Error('synthetic_read_failure');
    const searched = sessions.filter(session =>
      !query || `${session.title} ${session.working_directory}`.toLowerCase().includes(query.toLowerCase()),
    );
    const filtered = searched.filter(session => !source || session.source === source).sort((left, right) => {
      if (sort === 'oldest') return left.updated_at.localeCompare(right.updated_at) || left.id.localeCompare(right.id);
      if (sort === 'title') return left.title.localeCompare(right.title) || left.id.localeCompare(right.id);
      return right.updated_at.localeCompare(left.updated_at) || left.id.localeCompare(right.id);
    });
    const offset = Number(cursor ?? 0);
    const page = filtered.slice(offset, offset + 50);
    return {
      sessions: page,
      total: filtered.length,
      next_cursor: offset + 50 < filtered.length ? String(offset + 50) : null,
      sources: [
        ...historySources.map(source => ({
          source,
          availability: source === 'claude_code' || source === 'opencode' ? 'partial' as const : 'available' as const,
          session_count: searched.filter(session => session.source === source).length,
          ...(source === 'claude_code' || source === 'opencode' ? { error_code: 'scan_partial' } : {}),
        })),
      ],
    };
  }, []);

  const loadTranscript = useCallback(async (
    _projectId: string,
    sessionId: string,
    cursor?: string,
  ): Promise<WorkspaceSessionTranscriptPage> => {
    window.dispatchEvent(new CustomEvent('session-history-fixture-read', {
      detail: { sessionId, cursor: cursor ?? null },
    }));
    if (cursor == null) {
      latestTranscriptReads.current += 1;
      if (new URLSearchParams(window.location.search).has('transcript-error-once') && latestTranscriptReads.current === 2) {
        throw new Error('synthetic_transcript_read_failure');
      }
    }
    const messages = Array.from({ length: 101 }, (_, seq) => ({
      id: String(seq),
      role: seq % 2 === 0 ? 'user' as const : 'assistant' as const,
      content: `Synthetic message ${String(seq).padStart(3, '0')}\nFixture conversation text`,
      timestamp: new Date(Date.UTC(2026, 10, 11, 10, seq)).toISOString(),
      seq,
    }));
    if (params.has('missing-message') && cursor === '0') messages.splice(0, 1);
    const through = cursor == null ? 100 : Number(cursor);
    const candidates = messages.filter(message => message.seq <= through);
    const rawPage = candidates.slice(-51);
    const hasOlder = rawPage.length > 50;
    const page = hasOlder ? rawPage.slice(1) : rawPage;
    const source = sessionId.split(':', 1)[0];
    return {
      session_id: sessionId,
      through_seq: source === 'claude_code' || source === 'codex' ? null : through,
      messages: page.map(({ id, role, content, timestamp }) => ({ id, role, content, timestamp,
        ...((source === 'claude_code' || source === 'codex') && id === '2' ? { files: [{ path: 'src/synthetic-file.ts', operation: 'Edit', kind: 'fragment' as const, before: '<script>old()</script>', after: 'new()', truncated: false }] } : {}),
      })),
      next_cursor: hasOlder ? String(page[0].seq - 1) : null,
    };
  }, []);

  return (
    <main style={{ maxWidth: 1080, height: '100dvh', margin: '0 auto', padding: 24, display: 'flex', flexDirection: 'column', minHeight: 0 }}>
      <h2 style={{ margin: '0 0 8px', fontSize: 18, flexShrink: 0 }}>Fixture Project</h2>
      <p style={{ margin: '0 0 20px', color: 'var(--text-muted)', fontSize: 12, flexShrink: 0 }}>合成数据 · 不连接 Tauri Runtime</p>
      <WorkspaceSessionHistoryPanel projectId="fixture-project" loadHistory={loadHistory} loadTranscript={loadTranscript} />
    </main>
  );
}

createRoot(document.getElementById('root')!).render(<App />);
