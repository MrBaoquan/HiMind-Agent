import { useCallback, useEffect, useRef, useState } from 'react';
import { Activity, CheckCircle2, Clock3, Plus, ShieldCheck } from 'lucide-react';
import { BusyIndicator } from '../components/BusyIndicator';
import { EmptyState } from '../components/Common';
import { agentApi, type MissionCreateInput, type MissionEvent, type MissionSnapshot, type MissionView } from '../services/agentApi';
import { formatError } from '../types';
import {
  eventLabel,
  missionProgress,
  missionStage,
  MISSION_STAGE_LABELS,
  MISSION_STATUS_LABELS,
  MISSION_TASK_STATUS_LABELS,
  progressDisclosure,
  taskTitle,
} from './missionView';

const ACTIVE_STATUSES = new Set(['draft', 'planned', 'awaiting_confirmation', 'active', 'waiting']);

type Props = {
  onOpenMission: (missionId: string) => void;
  selectedMissionId?: string;
  onBackToTasks?: () => void;
};

export function MissionTeamPanel({ onOpenMission, selectedMissionId = '', onBackToTasks }: Props) {
  const [missions, setMissions] = useState<MissionView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [createOpen, setCreateOpen] = useState(false);
  const [notice, setNotice] = useState('');

  const load = useCallback(async (silent = false) => {
    if (!silent) setLoading(true);
    try {
      setMissions(await agentApi.listMissions());
      setError('');
    } catch (cause) {
      setError(formatError(cause, '团队任务读取失败'));
    } finally {
      if (!silent) setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
    const timer = window.setInterval(() => {
      if (document.visibilityState !== 'hidden') void load(true);
    }, 5000);
    return () => window.clearInterval(timer);
  }, [load]);

  if (selectedMissionId) {
    return <MissionDetail missionId={selectedMissionId} onBack={onBackToTasks || (() => onOpenMission(''))} onRefresh={load} />;
  }

  const active = missions.filter(mission => ACTIVE_STATUSES.has(mission.status));
  const history = missions.filter(mission => !ACTIVE_STATUSES.has(mission.status));

  return (
    <section className="mission-team-panel" aria-label="专家团队任务">
      <div className="mission-team-heading">
        <div>
          <span className="mission-team-kicker"><ShieldCheck size={15} />专家团队</span>
          <h2>持续跟进的团队任务</h2>
          <p>协调 Agent 提交计划；你确认后才会派发、验收和跟进返工。</p>
        </div>
        <button type="button" className="btn btn-primary" onClick={() => { setNotice(''); setCreateOpen(true); }}>
          <Plus size={15} />新建团队任务
        </button>
      </div>
      {notice ? <p className="mission-team-notice" role="status">{notice}</p> : null}
      {error ? <p className="error mission-team-error" role="alert">{error}</p> : null}
      {loading ? <div className="mission-team-loading"><BusyIndicator size={18} />正在读取团队任务</div> : null}
      {!loading && !missions.length ? (
        <EmptyState icon={Activity} title="还没有团队任务" text="先创建目标，再由专家团队提交计划。" />
      ) : null}
      {active.length ? <MissionList title="进行中" missions={active} onOpenMission={onOpenMission} /> : null}
      {history.length ? <MissionList title="已结束" missions={history} onOpenMission={onOpenMission} /> : null}
      {createOpen ? <CreateMissionDialog onClose={() => setCreateOpen(false)} onCreated={mission => {
        setCreateOpen(false);
        setNotice(`「${mission.title}」已创建。下一步需要填写或由协调 Agent 生成计划后才能确认。`);
        void load(true);
        onOpenMission(mission.mission_id);
      }} /> : null}
    </section>
  );
}

function MissionList({ title, missions, onOpenMission }: { title: string; missions: MissionView[]; onOpenMission: (id: string) => void }) {
  return (
    <div className="mission-team-list">
      <div className="mission-team-list-heading"><h3>{title}</h3><span>{missions.length} 项</span></div>
      {missions.map(mission => {
        const progress = missionProgress(mission);
        return (
          <button type="button" className="mission-team-row" key={mission.mission_id} onClick={() => onOpenMission(mission.mission_id)}>
            <span className={`mission-team-row-icon status-${mission.status}`} aria-hidden="true"><Activity size={16} /></span>
            <span className="mission-team-row-main">
              <strong>{mission.title}</strong>
              <small>{MISSION_STAGE_LABELS[missionStage(mission)]} · {progress.done}/{progress.total || 0} 任务完成</small>
            </span>
            <span className={`mission-team-status status-${mission.status}`}>{MISSION_STATUS_LABELS[mission.status]}</span>
          </button>
        );
      })}
    </div>
  );
}

function CreateMissionDialog({ onClose, onCreated }: { onClose: () => void; onCreated: (mission: MissionView) => void }) {
  const [title, setTitle] = useState('');
  const [objective, setObjective] = useState('');
  const [root, setRoot] = useState('');
  const [writeScope, setWriteScope] = useState('');
  const [acceptance, setAcceptance] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  async function submit() {
    if (busy) return;
    setBusy(true);
    setError('');
    try {
      const input: MissionCreateInput = {
        title: title.trim(), objective: objective.trim(),
        workspace: {
          root: root.trim(), read_scope: writeScope.split(/[,\n]/).map(value => value.trim()).filter(Boolean),
          write_scope: writeScope.split(/[,\n]/).map(value => value.trim()).filter(Boolean),
        },
        acceptance_criteria: acceptance.split('\n').map(value => value.trim()).filter(Boolean),
      };
      onCreated(await agentApi.createMission(input));
    } catch (cause) {
      setError(formatError(cause, '团队任务创建失败'));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="modal-backdrop" role="presentation" onClick={event => { if (event.currentTarget === event.target) onClose(); }}>
      <section className="mission-create-dialog" role="dialog" aria-modal="true" aria-labelledby="mission-create-title">
        <header><div><span>新建团队任务</span><h2 id="mission-create-title">先说清目标，再让 Agent 组队</h2></div><button type="button" className="btn" onClick={onClose}>关闭</button></header>
        <div className="mission-create-fields">
          <label><span>任务标题 *</span><input value={title} onChange={event => setTitle(event.target.value)} placeholder="例如：修复设置页焦点丢失" /></label>
          <label><span>目标</span><textarea value={objective} onChange={event => setObjective(event.target.value)} placeholder="完成后应该看到什么结果？" /></label>
          <label><span>工作区根目录 *</span><input value={root} onChange={event => setRoot(event.target.value)} placeholder="项目的本地绝对路径" /></label>
          <label><span>允许修改的范围 *</span><textarea value={writeScope} onChange={event => setWriteScope(event.target.value)} placeholder="例如：frontend/src，行或逗号分隔" /></label>
          <label><span>完成标准</span><textarea value={acceptance} onChange={event => setAcceptance(event.target.value)} placeholder="每行一条可验证标准" /></label>
        </div>
        {error ? <p className="error" role="alert">{error}</p> : null}
        <footer><small>创建后先确认计划，不会立即改代码。</small><button type="button" className="btn btn-primary" disabled={busy || !title.trim() || !root.trim() || !writeScope.trim()} onClick={() => void submit()}>{busy ? <BusyIndicator size={14} /> : <Plus size={14} />}{busy ? '创建中' : '创建任务'}</button></footer>
      </section>
    </div>
  );
}

function MissionDetail({ missionId, onBack, onRefresh }: { missionId: string; onBack: () => void; onRefresh: () => Promise<void> }) {
  const [mission, setMission] = useState<MissionView | null>(null);
  const [events, setEvents] = useState<MissionEvent[]>([]);
  const cursorRef = useRef(0);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [dispatching, setDispatching] = useState(false);
  const [planning, setPlanning] = useState(false);
  const [followingUp, setFollowingUp] = useState(false);

  const poll = useCallback(async (reset = false) => {
    try {
      const snapshot: MissionSnapshot = await agentApi.missionSnapshot(missionId, reset ? 0 : cursorRef.current);
      setMission(snapshot.mission);
      setEvents(current => reset ? snapshot.events : mergeEvents(current, snapshot.events));
      cursorRef.current = snapshot.cursor;
      setError('');
    } catch (cause) {
      setError(formatError(cause, '团队任务详情读取失败'));
    }
  }, [missionId]);

  useEffect(() => {
    cursorRef.current = 0; setEvents([]); void poll(true);
    const timer = window.setInterval(() => {
      if (document.visibilityState !== 'hidden') void poll();
    }, 5000);
    return () => window.clearInterval(timer);
  }, [missionId, poll]);

  if (!mission) return <section className="mission-detail"><button type="button" className="btn" onClick={onBack}>返回团队任务</button>{error ? <p className="error">{error}</p> : <div className="mission-team-loading"><BusyIndicator size={18} />正在读取任务详情</div>}</section>;
  const stage = missionStage(mission);
  const progress = missionProgress(mission);

  async function confirmPlan() {
    if (!mission || confirming) return;
    setConfirming(true);
    setError('');
    try {
      const selected = mission.proposed_tasks.map(task => task.id);
      await agentApi.confirmMissionPlan(mission.mission_id, mission.plan_sha256, selected, mission.workspace.write_scope, '本机用户');
      await poll(true);
      await onRefresh();
    } catch (cause) {
      setError(formatError(cause, '计划确认失败'));
    } finally {
      setConfirming(false);
    }
  }

  async function dispatchNext() {
    if (dispatching) return;
    setDispatching(true);
    setError('');
    try {
      const result = await agentApi.dispatchNextMissionTask(missionId);
      if (!result.accepted && !result.dispatched && result.reason) setError(result.reason);
      if (result.accepted) setError('');
      await poll(true);
      await onRefresh();
    } catch (cause) {
      setError(formatError(cause, '任务派发失败'));
      await poll(true);
    } finally {
      setDispatching(false);
    }
  }

  async function generatePlan() {
    if (planning) return;
    setPlanning(true);
    setError('');
    try {
      await agentApi.generateMissionPlan(missionId);
      await poll(true);
      await onRefresh();
    } catch (cause) {
      setError(formatError(cause, '计划生成失败'));
    } finally {
      setPlanning(false);
    }
  }

  async function generateFollowUp() {
    if (followingUp) return;
    setFollowingUp(true);
    setError('');
    try {
      await agentApi.generateMissionFollowUp(missionId);
      await poll(true);
      await onRefresh();
    } catch (cause) {
      setError(formatError(cause, '跟进任务生成失败'));
    } finally {
      setFollowingUp(false);
    }
  }

  return (
    <section className="mission-detail">
      <div className="mission-detail-head"><button type="button" className="btn" onClick={onBack}>返回团队任务</button><span className={`mission-team-status status-${mission.status}`}>{MISSION_STATUS_LABELS[mission.status]}</span></div>
      <header className="mission-detail-hero"><div><span className="mission-team-kicker"><ShieldCheck size={15} />专家团队任务</span><h2>{mission.title}</h2><p>{mission.objective || '尚未填写目标说明'}</p></div><div className="mission-detail-progress"><strong>{progress.done}/{progress.total || 0}</strong><span>任务完成</span><div className="task-center-progress"><span style={{ width: progress.total ? `${Math.round(progress.done / progress.total * 100)}%` : '0%' }} /></div></div></header>
      <div className="mission-stage-rail" aria-label="团队任务阶段">{['review', 'confirm', 'optimize', 'accept'].map((item, index) => { const itemStage = item as typeof stage; const active = itemStage === stage; const passed = ['review', 'confirm', 'optimize', 'accept'].indexOf(stage) > index; return <div key={item} className={active ? 'active' : passed ? 'passed' : ''}><span>{passed ? <CheckCircle2 size={14} /> : index + 1}</span><strong>{MISSION_STAGE_LABELS[itemStage]}</strong></div>; })}</div>
      {error ? <p className="error" role="alert">{error}</p> : null}
      <div className="mission-detail-grid">
        <section className="mission-detail-panel"><div className="mission-detail-panel-head"><h3>计划与确认</h3><span>r{mission.plan_revision || 0}</span></div>{mission.plan_summary ? <p>{mission.plan_summary}</p> : <p className="muted">还没有执行计划，确认前不会派发写任务。</p>}{mission.status === 'draft' ? <button type="button" className="btn" disabled={planning} onClick={() => void generatePlan()}>{planning ? <BusyIndicator size={14} /> : <ShieldCheck size={14} />}{planning ? '协调中' : '生成执行计划'}</button> : null}{mission.confirmation ? <div className="mission-confirmation"><strong>已确认</strong><small>{mission.confirmation.confirmed_by} · {mission.confirmation.approved_paths.join('、')}</small><code title={mission.confirmation.plan_sha256}>{mission.confirmation.plan_sha256.slice(0, 16)}…</code></div> : mission.plan_sha256 ? <div className="mission-confirmation is-pending"><strong>等待确认</strong><small>计划摘要 {mission.plan_sha256.slice(0, 16)}…</small></div> : null}{mission.status === 'awaiting_confirmation' && mission.plan_sha256 ? <button type="button" className="btn btn-primary" disabled={confirming} onClick={() => void confirmPlan()}>{confirming ? <BusyIndicator size={14} /> : <CheckCircle2 size={14} />}{confirming ? '确认中' : '确认计划并允许派发'}</button> : null}</section>
        <section className="mission-detail-panel"><div className="mission-detail-panel-head"><h3>{mission.status === 'awaiting_confirmation' ? '待确认任务' : '任务派发'}</h3><span>{(mission.status === 'awaiting_confirmation' ? mission.proposed_tasks : mission.tasks.map(task => task.spec)).length} 项</span></div>{mission.status === 'awaiting_confirmation' && mission.proposed_tasks.length ? <div className="mission-task-list">{mission.proposed_tasks.map(task => <div className="mission-task-row task-proposed" key={task.id}><span className="mission-task-status">待确认</span><div><strong>{task.title}</strong><small>{task.execution.role_id || '未指定角色'} · {task.execution.executor}{task.execution.model ? ` · ${task.execution.model}` : ''}</small>{task.depends_on.length ? <em>依赖：{task.depends_on.join('、')}</em> : null}{task.write_paths.length ? <em>写入：{task.write_paths.join('、')}</em> : null}{task.acceptance_criteria.length ? <em>验收：{task.acceptance_criteria.join('；')}</em> : null}</div></div>)}</div> : mission.tasks.length ? <div className="mission-task-list">{mission.tasks.map(task => <div className={`mission-task-row task-${task.status}`} key={task.spec.id}><span className="mission-task-status">{MISSION_TASK_STATUS_LABELS[task.status]}</span><div><strong>{task.spec.title}</strong><small>{task.spec.execution.role_id || '未指定角色'} · {task.spec.execution.executor}{task.spec.execution.model ? ` · ${task.spec.execution.model}` : ''}</small>{task.spec.depends_on.length ? <em>依赖：{task.spec.depends_on.map(id => taskTitle(mission, id)).join('、')}</em> : null}{task.status === 'failed' ? <em>验收未通过，等待跟进任务</em> : null}</div></div>)}</div> : <p className="muted">确认计划后会在这里出现实际任务；空计划不会派发 Agent。</p>}{mission.status === 'active' && mission.tasks.some(task => task.status === 'failed') ? <button type="button" className="btn" disabled={followingUp} onClick={() => void generateFollowUp()}>{followingUp ? <BusyIndicator size={14} /> : <Activity size={14} />}{followingUp ? '协调中' : '生成跟进任务'}</button> : null}</section>
      </div>
      <section className="mission-detail-panel mission-timeline-panel"><div className="mission-detail-panel-head"><div><h3>执行过程</h3><small>{progressDisclosure(mission, events)}</small></div><span className="mission-timeline-actions">{mission.status === 'active' && mission.tasks.some(task => task.status === 'ready') ? <button type="button" className="btn btn-primary" disabled={dispatching} onClick={() => void dispatchNext()}>{dispatching ? <BusyIndicator size={14} /> : <Plus size={14} />}{dispatching ? '执行中' : '派发下一个任务'}</button> : null}<button type="button" className="btn" onClick={() => { setBusy(true); void poll(true).finally(() => setBusy(false)); }}>{busy ? <BusyIndicator size={14} /> : <Clock3 size={14} />}刷新</button></span></div>{events.length ? <ol className="mission-timeline">{events.map(event => <li key={event.event_id}><span className={`mission-timeline-dot type-${event.event_type}`} aria-hidden="true" /><div><strong>{eventLabel(event)}{event.task_id ? ` · ${taskTitle(mission, event.task_id)}` : ''}</strong><p>{event.summary}</p><time dateTime={event.occurred_at}>{new Date(event.occurred_at).toLocaleString()}</time></div></li>)}</ol> : <p className="muted">暂无细粒度执行事件，只显示任务状态。</p>}</section>
      <div className="mission-detail-foot"><span>确认哈希 {mission.plan_sha256 ? `${mission.plan_sha256.slice(0, 12)}…` : '尚未生成'}</span><span>{mission.budget ? `预算 ${mission.budget.max_rounds} 轮 / ${mission.budget.max_minutes} 分钟 / ${mission.budget.max_repair_attempts} 次返工` : '预算待计划生成'}</span></div>
    </section>
  );
}

function mergeEvents(current: MissionEvent[], next: MissionEvent[]) {
  const byId = new Map(current.map(event => [event.event_id, event]));
  for (const event of next) byId.set(event.event_id, event);
  return [...byId.values()].sort((left, right) => left.sequence - right.sequence);
}
