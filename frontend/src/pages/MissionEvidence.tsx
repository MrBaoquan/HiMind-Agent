import type { MissionEvent, MissionTaskView, MissionView } from '../services/agentApi';
import { eventLabel, missionDeliveries, missionEvidencePlanCompatible } from './missionView';

const body = (value: unknown) => value == null || value === '' ? '未提供正文' : typeof value === 'string' ? value : JSON.stringify(value, null, 2);
const object = (value: unknown): Record<string, unknown> | null => value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : null;
const sameCandidate = (left: Record<string, unknown>, right: Record<string, unknown>) =>
  ['schema_version','identity_kind','workspace_root','candidate_id','tree_digest','status_digest'].every(key => left[key] === right[key]);
// Workspace membership only: never normalize candidate identity fields or infer
// filesystem identity (symlinks, aliases, case-sensitive directories, etc.).
function windowsWorkspaceSpelling(value: string): string | null {
  const extended = value.startsWith('\\\\?\\');
  const path = extended ? value.slice(4) : value;
  if (!/^[a-zA-Z]:[\\/]/.test(path) || (extended && path.includes('/'))) return null;
  const parts = path.slice(3).split(/[\\/]/);
  if (parts[parts.length - 1] === '') parts.pop();
  // Extended paths bypass Win32 normalization. Only unambiguous, ordinary
  // components can be compared across those two namespaces.
  if (parts.some(part => !part || part === '.' || part === '..' || /[<>:"|?*\x00-\x1f]/.test(part)
    || /[. ]$/.test(part) || /^(con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i.test(part))) return null;
  return path[0].toUpperCase() + ':/' + parts.join('/');
}
function sameWorkspaceRoot(value: unknown, workspace: string): boolean {
  if (value === workspace) return true;
  if (typeof value !== 'string') return false;
  const left = windowsWorkspaceSpelling(value), right = windowsWorkspaceSpelling(workspace);
  return left !== null && right !== null && left === right;
}
function candidate(value: unknown, workspace: string) {
  const record = object(value);
  return record?.schema_version === 'mission_candidate.v1' && record.identity_kind === 'scoped_files_sha256'
    && sameWorkspaceRoot(record.workspace_root, workspace)
    && ['candidate_id','tree_digest','status_digest'].every(key => typeof record[key] === 'string' && /^[a-f0-9]{64}$/.test(record[key] as string)) ? record : null;
}
function EvidenceRecord({title, run, event, children}: {title: string; run: string; event?: MissionEvent; children?: React.ReactNode}) {
  return <section className="mission-evidence-record"><h4>{title}</h4><dl>
    <div><dt>运行</dt><dd><code>{run || '未提供运行号'}</code></dd></div>
    <div><dt>记录</dt><dd><code>{event?.event_id || '未提供记录 ID'}</code></dd></div>
    <div><dt>时间</dt><dd><time dateTime={event?.occurred_at}>{event?.occurred_at || '未提供时间'}</time></dd></div>
  </dl>{children || <p className="muted">未提供正文</p>}</section>;
}
function AttemptEvidence({task, events, missionId, plan, workspace, entries = false}: {task: MissionTaskView; events: MissionEvent[]; missionId: string; plan: string; workspace: string; entries?: boolean}) {
  const matching = events.filter(event => event.mission_id === missionId && event.task_id === task.spec.id);
  const implementation = task.attempt_run_id ? [...matching].reverse().find(event => event.event_type === 'task_awaiting_verification' && event.payload.run_id === task.attempt_run_id) : undefined;
  const distinctRuns = Boolean(task.attempt_run_id && task.verification_run_id && task.attempt_run_id !== task.verification_run_id);
  const rawChecked = distinctRuns ? [...matching].reverse().find(event => event.event_type === 'host_verification_checked'
    && missionEvidencePlanCompatible({mission_id:missionId,plan_sha256:plan} as MissionView, task, event.payload.plan_sha256, events)
    && event.payload.implementation_run_id === task.attempt_run_id && event.payload.verification_run_id === task.verification_run_id) : undefined;
  const started = distinctRuns ? [...matching].reverse().find(event => event.event_type === 'verification_started'
    && event.payload.implementation_run_id === task.attempt_run_id && event.payload.verification_run_id === task.verification_run_id) : undefined;
  // Checks are persisted before the independent reviewer runs (also on pause).
  // Never substitute an older preparation, another retry, or an ambiguous candidate.
  const currentRun = matching.filter(event => event.payload.implementation_run_id === task.attempt_run_id && Boolean(task.attempt_run_id));
  const floor = Math.max(implementation?.sequence ?? 0, ...currentRun.filter(event => ['verification_preparing','verification_retry_requested'].includes(event.event_type)).map(event => event.sequence));
  const completed = currentRun.filter(event => event.event_type === 'verification_checks_completed' && event.sequence > floor).sort((a,b) => a.sequence - b.sequence);
  const latest = completed[completed.length - 1];
  const boundary = object(latest?.payload.host_check_boundary);
  const before = candidate(boundary?.before, workspace);
  const after = candidate(boundary?.after, workspace);
  const ambiguous = completed.some(event => {
    const previous = candidate(object(event.payload.host_check_boundary)?.after, workspace);
    return !previous || !after || !sameCandidate(previous, after);
  });
  const verdictCandidate = candidate(rawChecked?.payload.candidate_before, workspace);
  const boundaryKnown = boundary?.source === 'host_required_check_comparison' && typeof boundary.matches === 'boolean'
    && before && after && !ambiguous && (boundary.matches === false || sameCandidate(before, after));
  const checked = rawChecked && (!latest || (boundaryKnown && verdictCandidate && sameCandidate(verdictCandidate, after))) ? rawChecked : undefined;
  const checkEvent = latest ? boundaryKnown ? latest : undefined : checked;
  const checks = Array.isArray(checkEvent?.payload.checks) ? checkEvent.payload.checks.filter(item => object(item)) as Record<string,unknown>[] : null;
  const records = [
    {title:'实现结果', content:
      <EvidenceRecord title="实现结果" run={task.attempt_run_id} event={implementation}>
        <p>{implementation?.summary || '未提供实现摘要'}</p><pre>{body(implementation?.payload.result ?? implementation?.payload.output)}</pre>
      </EvidenceRecord>},
    {title:'必需检查', content:
      <EvidenceRecord title="必需检查" run={task.attempt_run_id} event={checkEvent}>
        <p>任务：<code>{task.spec.id}</code> · implementation_run_id：<code>{task.attempt_run_id || '未提供'}</code></p>
        {latest && boundaryKnown ? <><p>检查时冻结候选：<code>{String(after?.candidate_id)}</code></p>
          <p>检查前候选：<code>{String(before?.candidate_id)}</code> · 检查后候选：<code>{String(after?.candidate_id)}</code></p>
          <p>{boundary?.matches === true ? '宿主记录检查前后候选一致' : '检查前后候选发生变化，不能作为通过依据'}；仅关联本次准备记录，不推断当前文件仍相同。</p></>
          : <p className="muted">{latest ? <>检查候选边界缺失、失配或无法确认。<br />未串用其他候选，原始记录可在历史中查看。</> : <>未提供检查候选边界。<br />已有独立验收记录仅按任务与运行关联展示。</>}</p>}
        <p>宿主必需检查与独立验收分别记录。<br />检查全绿不等于独立验收通过。</p>
        {checks?.length ? checks.map((check, index) => <div key={index}><code>{String(check.command || '未提供命令')}</code>
          <p>{typeof check.exit_code === 'number' ? `退出码 ${check.exit_code}` : '未提供退出码'}</p>
          <pre>{body(check.test_summary)}</pre>{check.error ? <p>{String(check.error)}</p> : null}
          <small>数值 TAP 总数：未提供，不推测通过数量。</small>
          <small>输出摘要：<code>{String(check.output_sha256 || '未提供')}</code></small>
        </div>) : <p className="muted">未提供检查结果；不推测通过数量</p>}
      </EvidenceRecord>},
    {title:'独立验收', content:
      <EvidenceRecord title="独立验收" run={task.verification_run_id} event={checked || started}>
        <pre>{body(checked?.payload.reviewer_output)}</pre>
        {checked?.payload.reason ? <p>{String(checked.payload.reason)}</p> : null}
        {!checked ? <p className="muted">未提供关联到本次实现与核验运行的验收结果</p> : null}
      </EvidenceRecord>},
  ];
  return <div className="mission-attempt-evidence" data-attempt-id={task.spec.id}>
    <p><strong>{task.spec.title}</strong> · {task.spec.repair_of ? `重做 ${task.spec.repair_of} 的交付` : '原始交付'} · {checked?.payload.passed === true ? '本次运行的独立验收已记录通过' : checked?.payload.passed === false ? '本次验收未通过，保留失败证据' : '本次运行尚无关联验收结论'}</p>
    <div className={entries ? 'mission-task-evidence-entries' : 'mission-evidence-columns'}>
      {records.map(record => entries ? <details key={record.title} className="mission-task-evidence-entry"><summary>{record.title}</summary>{record.content}</details> : <div key={record.title}>{record.content}</div>)}
    </div>
    {task.evidence_refs.length ? <ul>{task.evidence_refs.map((ref,index) => <li key={index}><code className="task-evidence-path">{ref}</code></li>)}</ul> : <p className="muted">未提供证据引用</p>}
    <details className="mission-evidence-history"><summary>历史记录与失败证据（{matching.length} 条已读取记录）</summary>
      {matching.map(event => <article key={event.event_id}><strong>{eventLabel(event)}</strong><p>{event.summary}</p><code>{event.event_id} · {event.occurred_at}</code>
        <pre>{JSON.stringify(event.payload, null, 2)}</pre></article>)}
    </details>
  </div>;
}

/** Same records as overview, scoped to this row; lineage never substitutes a sibling's verdict. */
export function MissionTaskEvidence({mission, task, events}: {mission: MissionView; task: MissionTaskView; events: MissionEvent[]}) {
  const group = missionDeliveries(mission).groups.find(group => group.attempts.some(attempt => attempt.spec.id === task.spec.id));
  return <div className="mission-task-evidence">
    <AttemptEvidence task={task} events={events} missionId={mission.mission_id} plan={mission.plan_sha256} workspace={mission.workspace.root} entries/>
    {group && group.attempts.length > 1 ? <details className="mission-evidence-history mission-task-lineage"><summary>返工谱系与历史（{group.attempts.length} 次尝试）</summary>
      {group.attempts.map(attempt => <AttemptEvidence key={attempt.spec.id} task={attempt} events={events} missionId={mission.mission_id} plan={mission.plan_sha256} workspace={mission.workspace.root}/>)}
    </details> : !group ? <p className="muted">返工归属未确认：父任务缺失或关联成环，未串用其他交付记录。</p> : null}
  </div>;
}
export function MissionEvidence({mission, events}: {mission: MissionView; events: MissionEvent[]}) {
  const {groups, unassociated} = missionDeliveries(mission);
  return <section className="mission-detail-panel mission-delivery"><h3>交付结果与验收</h3>
    <p>{mission.status === 'completed' ? '任务已完成。' : '总体尚未完成；已有交付与失败记录可先查看。'}实现、系统必需检查、独立验收分别列出。</p>
    {groups.length ? groups.map(group => <details key={group.root.spec.id} open={groups.length === 1} className="mission-delivery-group">
      <summary>{group.root.spec.title} · {group.done ? '交付已通过验收' : '交付尚未完成'} · {group.attempts.length} 次尝试</summary>
      {group.attempts.map(task => <AttemptEvidence key={task.spec.id} task={task} events={events} missionId={mission.mission_id} plan={mission.plan_sha256} workspace={mission.workspace.root}/>)}
    </details>) : <p className="muted">尚无可展示的交付记录</p>}
    {unassociated.length ? <details><summary>归属未确认的尝试（{unassociated.length}）</summary><p>父任务缺失或关联成环，未计入交付进度。</p>{unassociated.map(task => <AttemptEvidence key={task.spec.id} task={task} events={events} missionId={mission.mission_id} plan={mission.plan_sha256} workspace={mission.workspace.root}/>)}</details> : null}
  </section>;
}
