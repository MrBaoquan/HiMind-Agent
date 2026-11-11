import { useCallback, useEffect, useRef, useState } from 'react';
import { Activity, CheckCircle2, Clock3, Download, Plus, ShieldCheck } from 'lucide-react';
import { BusyIndicator } from '../components/BusyIndicator';
import { EmptyState } from '../components/Common';
import { agentApi, type MissionAssignmentInput, type MissionAssignmentOptions, type MissionCreateInput, type MissionEvent, type MissionExecutionOption, type MissionRoleAssignment, type MissionSnapshot, type MissionTaskSpec, type MissionView } from '../services/agentApi';
import { formatError } from '../types';
import {
  eventLabel,
  missionNextAction,
  missionProgress,
  missionStage,
  MISSION_STAGE_LABELS,
  MISSION_STATUS_LABELS,
  MISSION_TASK_STATUS_LABELS,
  progressDisclosure,
  taskTitle,
} from './missionView';

const COLLAB_ACTIVE_STATUSES = new Set(['draft', 'awaiting_assignment', 'planned', 'awaiting_confirmation', 'active', 'waiting']);

type Props = {
  onOpenMission: (missionId: string) => void;
  selectedMissionId?: string;
  onBackToTasks?: () => void;
};

export function CollaborationTaskPanel({ onOpenMission, selectedMissionId = '', onBackToTasks }: Props) {
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
      setError(formatError(cause, '协作任务读取失败'));
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
    return <CollaborationTaskDetail missionId={selectedMissionId} onBack={onBackToTasks || (() => onOpenMission(''))} onRefresh={load} />;
  }

  const active = missions.filter(mission => COLLAB_ACTIVE_STATUSES.has(mission.status));
  const history = missions.filter(mission => !COLLAB_ACTIVE_STATUSES.has(mission.status));

  return (
    <section className="collaboration-task-panel" aria-label="协作任务">
      <div className="collaboration-task-heading">
        <div>
          <span className="collaboration-task-kicker"><ShieldCheck size={15} />协作任务</span>
          <h2>可跟踪、可指派的协作任务</h2>
          <p>角色来自固定工作流；配置、确认、授权分开。</p>
        </div>
        <button type="button" className="btn btn-primary" onClick={() => { setNotice(''); setCreateOpen(true); }}>
          <Plus size={15} />新建协作任务
        </button>
      </div>
      {notice ? <p className="collaboration-task-notice" role="status">{notice}</p> : null}
      {error ? <p className="error collaboration-task-error" role="alert">{error}</p> : null}
      {loading ? <div className="collaboration-task-loading"><BusyIndicator size={18} />正在读取协作任务</div> : null}
      {!loading && !missions.length ? (
        <EmptyState icon={Activity} title="还没有协作任务" text="先创建目标，再由协作任务提交计划。" />
      ) : null}
      {active.length ? <CollaborationTaskList title="进行中" missions={active} onOpenMission={onOpenMission} /> : null}
      {history.length ? <CollaborationTaskList title="已结束" missions={history} onOpenMission={onOpenMission} /> : null}
      {createOpen ? <CreateMissionDialog onClose={() => setCreateOpen(false)} onCreated={mission => {
        setCreateOpen(false);
        setNotice(`「${mission.title}」已创建。下一步是指派角色；计划确认和路径授权随后单独完成。`);
        void load(true);
        onOpenMission(mission.mission_id);
      }} /> : null}
    </section>
  );
}

function CollaborationTaskList({ title, missions, onOpenMission }: { title: string; missions: MissionView[]; onOpenMission: (id: string) => void }) {
  return (
    <div className="collaboration-task-list">
      <div className="collaboration-task-list-heading"><h3>{title}</h3><span>{missions.length} 项</span></div>
      {missions.map(mission => {
        const progress = missionProgress(mission);
        return (
          <button type="button" className="collaboration-task-row" key={mission.mission_id} onClick={() => onOpenMission(mission.mission_id)}>
            <span className={`collaboration-task-row-icon status-${mission.status}`} aria-hidden="true"><Activity size={16} /></span>
            <span className="collaboration-task-row-main">
              <strong>{mission.title}</strong>
              <small>{MISSION_STAGE_LABELS[missionStage(mission)]} · {progress.done}/{progress.total || 0} 任务完成</small>
              <em>{missionNextAction(mission)}</em>
            </span>
            <span className={`collaboration-task-status status-${mission.status}`}>{MISSION_STATUS_LABELS[mission.status]}</span>
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
      setError(formatError(cause, '协作任务创建失败'));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="modal-backdrop" role="presentation" onClick={event => { if (event.currentTarget === event.target) onClose(); }}>
      <section className="collaboration-create-dialog" role="dialog" aria-modal="true" aria-labelledby="mission-create-title">
        <header><div><span>新建协作任务</span><h2 id="mission-create-title">先说清目标和范围，再选择协作工作流</h2></div><button type="button" className="btn" onClick={onClose}>关闭</button></header>
        <div className="collaboration-create-fields">
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

function TaskExecutionEditor({ mission, task, options, onSaved }: {
  mission: MissionView;
  task: MissionTaskSpec;
  options: MissionExecutionOption[];
  onSaved: (mission: MissionView) => void;
}) {
  const [executor, setExecutor] = useState(task.execution.executor);
  const [model, setModel] = useState(task.execution.model);
  const [roleId, setRoleId] = useState(task.execution.role_id);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const selected = options.find(option => option.executor === executor);
  const models = selected?.models ?? [];

  useEffect(() => {
    setExecutor(task.execution.executor);
    setModel(task.execution.model);
    setRoleId(task.execution.role_id);
  }, [task.execution.executor, task.execution.model, task.execution.role_id]);

  async function save() {
    if (saving) return;
    setSaving(true);
    setError('');
    try {
      const updated = await agentApi.updateMissionTaskExecution(
        mission.mission_id,
        task.id,
        mission.plan_sha256,
        { executor, model, role_id: roleId.trim() },
      );
      onSaved(updated);
    } catch (cause) {
      setError(formatError(cause, '执行绑定更新失败'));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="mission-execution-editor">
      <label><span>专家角色 ID</span><input value={roleId} onChange={event => setRoleId(event.target.value)} placeholder="例如 software.implementer" /></label>
      <label><span>执行器</span>
        <select value={executor} onChange={event => {
          const next = options.find(option => option.executor === event.target.value);
          setExecutor(event.target.value);
          setModel(next?.models[0]?.alias ?? '');
        }}>
          {options.map(option => <option key={option.executor} value={option.executor} disabled={!option.available}>
            {option.label}{option.available ? '' : `（${option.reason || '不可用'}）`}
          </option>)}
        </select>
      </label>
      <label><span>模型</span>
        <select value={model} onChange={event => setModel(event.target.value)}>
          <option value="">Runtime 当前模型</option>
          {models.map(option => <option key={option.alias} value={option.alias}>{option.label}</option>)}
        </select>
      </label>
      <button type="button" className="btn" disabled={saving || !executor || !roleId.trim()} onClick={() => void save()}>
        {saving ? <BusyIndicator size={14} /> : null}{saving ? '保存中' : '保存绑定'}
      </button>
      {error ? <small className="error">{error}</small> : null}
    </div>
  );
}

function CollaborationTaskDetail({ missionId, onBack, onRefresh }: { missionId: string; onBack: () => void; onRefresh: () => Promise<void> }) {
  const [mission, setMission] = useState<MissionView | null>(null);
  const [events, setEvents] = useState<MissionEvent[]>([]);
  const cursorRef = useRef(0);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [dispatching, setDispatching] = useState(false);
  const [planning, setPlanning] = useState(false);
  const [followingUp, setFollowingUp] = useState(false);
  const [controlling, setControlling] = useState(false);
  const [executionOptions, setExecutionOptions] = useState<MissionExecutionOption[]>([]);
  const [selectedTaskIds, setSelectedTaskIds] = useState<string[]>([]);
  const [approvedAbsoluteReadPaths, setApprovedAbsoluteReadPaths] = useState<string[]>([]);
  const [approvedAbsoluteWritePaths, setApprovedAbsoluteWritePaths] = useState<string[]>([]);
  const reviewPlanRef = useRef('');

  const poll = useCallback(async (reset = false) => {
    try {
      const snapshot: MissionSnapshot = await agentApi.missionSnapshot(missionId, reset ? 0 : cursorRef.current);
      if (snapshot.mission.status === 'awaiting_confirmation' && snapshot.mission.plan_sha256 && reviewPlanRef.current !== snapshot.mission.plan_sha256) {
        reviewPlanRef.current = snapshot.mission.plan_sha256;
        setSelectedTaskIds(snapshot.mission.proposed_tasks.map(task => task.id));
        setApprovedAbsoluteReadPaths([]);
        setApprovedAbsoluteWritePaths([]);
      }
      setMission(snapshot.mission);
      setEvents(current => reset ? snapshot.events : mergeEvents(current, snapshot.events));
      cursorRef.current = snapshot.cursor;
      setError('');
    } catch (cause) {
      setError(formatError(cause, '协作任务详情读取失败'));
    }
  }, [missionId]);

  useEffect(() => {
    void agentApi.listMissionExecutionOptions().then(setExecutionOptions).catch(cause => setError(formatError(cause, '执行器目录读取失败')));
  }, []);

  useEffect(() => {
    cursorRef.current = 0; setEvents([]); void poll(true);
    const timer = window.setInterval(() => {
      if (document.visibilityState !== 'hidden') void poll();
    }, 5000);
    return () => window.clearInterval(timer);
  }, [missionId, poll]);

  if (!mission) return <section className="mission-detail"><button type="button" className="btn" onClick={onBack}>返回协作任务</button>{error ? <p className="error">{error}</p> : <div className="collaboration-task-loading"><BusyIndicator size={18} />正在读取任务详情</div>}</section>;
  const stage = missionStage(mission);
  const progress = missionProgress(mission);

  async function confirmPlan() {
    if (!mission || confirming) return;
    setConfirming(true);
    setError('');
    try {
      const selected = eligibleMissionTasks(
        mission.proposed_tasks,
        approvedAbsoluteReadPaths,
        approvedAbsoluteWritePaths,
        selectedTaskIds,
      );
      const selectedIds = selected.map(task => task.id);
      if (!selectedIds.length) {
        throw new Error('至少选择一项已获路径授权的任务');
      }
      const selectedReadPaths = new Set(selected.flatMap(task => task.read_paths.filter(isAbsoluteMissionPath).map(missionPathKey)));
      const selectedWritePaths = new Set(selected.flatMap(task => task.write_paths.filter(isAbsoluteMissionPath).map(missionPathKey)));
      const readApprovals = approvedAbsoluteReadPaths.filter(path => selectedReadPaths.has(missionPathKey(path)));
      const writeApprovals = approvedAbsoluteWritePaths.filter(path => selectedWritePaths.has(missionPathKey(path)));
      await agentApi.confirmMissionPlan(
        mission.mission_id,
        mission.plan_sha256,
        selectedIds,
        mission.workspace.write_scope.filter(path => !isAbsoluteMissionPath(path)),
        readApprovals,
        writeApprovals,
        '本机用户',
      );
      await poll(true);
      await onRefresh();
    } catch (cause) {
      setError(formatError(cause, '计划确认失败'));
    } finally {
      setConfirming(false);
    }
  }

  async function controlMission(action: 'pause' | 'resume' | 'cancel') {
    if (controlling) return;
    setControlling(true);
    try {
      setMission(await agentApi.controlMission(missionId, action));
      await poll(true);
      await onRefresh();
    } catch (cause) { setError(formatError(cause, '任务状态更新失败')); }
    finally { setControlling(false); }
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
      <div className="mission-detail-head"><button type="button" className="btn" onClick={onBack}>返回协作任务</button><span className={`collaboration-task-status status-${mission.status}`}>{MISSION_STATUS_LABELS[mission.status]}</span></div>
      <header className="mission-detail-hero"><div><span className="collaboration-task-kicker"><ShieldCheck size={15} />协作任务</span><h2>{mission.title}</h2><p>{mission.objective || '尚未填写目标说明'}</p></div><div className="mission-detail-progress"><strong>{progress.done}/{progress.total || 0}</strong><span>任务完成</span><div className="task-center-progress"><span style={{ width: progress.total ? `${Math.round(progress.done / progress.total * 100)}%` : '0%' }} /></div></div></header>
      <div className="mission-stage-rail" aria-label="协作任务阶段">{['review', 'confirm', 'optimize', 'accept'].map((item, index) => { const itemStage = item as typeof stage; const active = itemStage === stage; const passed = ['review', 'confirm', 'optimize', 'accept'].indexOf(stage) > index; return <div key={item} className={active ? 'active' : passed ? 'passed' : ''}><span>{passed ? <CheckCircle2 size={14} /> : index + 1}</span><strong>{MISSION_STAGE_LABELS[itemStage]}</strong></div>; })}</div>
      {error ? <p className="error" role="alert">{error}</p> : null}
      <div className="mission-control-actions">
        {mission.status === 'active' ? <button type="button" className="btn" disabled={controlling} onClick={() => void controlMission('pause')}>暂停后续派发</button> : null}
        {mission.status === 'waiting' ? <button type="button" className="btn" disabled={controlling} onClick={() => void controlMission('resume')}>恢复任务</button> : null}
        {COLLAB_ACTIVE_STATUSES.has(mission.status) ? <button type="button" className="btn" disabled={controlling} onClick={() => void controlMission('cancel')}>取消任务</button> : null}
      </div>
      <div className="mission-detail-grid">
        <section className="mission-detail-panel"><div className="mission-detail-panel-head"><h3>计划与确认</h3><span>r{mission.plan_revision || 0}</span></div>{mission.plan_summary ? <p>{mission.plan_summary}</p> : <p className="muted">还没有执行计划，确认前不会派发写任务。</p>}{(mission.status === 'draft' || mission.status === 'planned') ? <button type="button" className="btn" disabled={planning} onClick={() => void generatePlan()}>{planning ? <BusyIndicator size={14} /> : <ShieldCheck size={14} />}{planning ? '协调中' : '生成执行计划'}</button> : null}{mission.confirmation ? <div className="mission-confirmation"><strong>已确认</strong><small>{mission.confirmation.confirmed_by} · 相对写入：{mission.confirmation.approved_paths.join('、') || '无'} · 绝对读取：{mission.confirmation.approved_absolute_read_paths.join('、') || '无'} · 绝对写入：{mission.confirmation.approved_absolute_write_paths.join('、') || '无'}</small><code title={mission.confirmation.plan_sha256}>{mission.confirmation.plan_sha256.slice(0, 16)}…</code></div> : mission.plan_sha256 ? <div className="mission-confirmation is-pending"><strong>等待确认</strong><small>计划摘要 {mission.plan_sha256.slice(0, 16)}…</small></div> : null}{mission.status === 'awaiting_confirmation' && mission.plan_sha256 ? <button type="button" className="btn btn-primary" disabled={confirming || eligibleMissionTasks(mission.proposed_tasks, approvedAbsoluteReadPaths, approvedAbsoluteWritePaths, selectedTaskIds).length === 0} onClick={() => void confirmPlan()}>{confirming ? <BusyIndicator size={14} /> : <CheckCircle2 size={14} />}{confirming ? '确认中' : '确认计划并允许派发'}</button> : null}</section>
        <section className="mission-detail-panel"><div className="mission-detail-panel-head"><h3>{mission.status === 'awaiting_confirmation' ? '待确认任务' : '任务派发'}</h3><span>{(mission.status === 'awaiting_confirmation' ? mission.proposed_tasks : mission.tasks.map(task => task.spec)).length} 项</span></div>{mission.status === 'awaiting_confirmation' && mission.proposed_tasks.length ? <div className="mission-task-list">{mission.proposed_tasks.map(task => <div className="mission-task-row task-proposed" key={task.id}><label className="mission-task-selection"><input type="checkbox" aria-label={`选择任务 ${task.title}`} checked={selectedTaskIds.includes(task.id)} onChange={() => setSelectedTaskIds(current => toggleMissionTaskSelection(mission.proposed_tasks, current, task.id))} /><span>纳入计划</span></label><div><strong>{task.title}</strong><small>{task.execution.role_id || '未指定角色'} · {task.execution.executor}{task.execution.model ? ` · ${task.execution.model}` : ''}</small><TaskExecutionEditor mission={mission!} task={task} options={executionOptions} onSaved={next => { setMission(next); void poll(true); void onRefresh(); }} />{task.depends_on.length ? <em>依赖：{task.depends_on.join('、')}</em> : null}{task.write_paths.length ? <em>写入：{task.write_paths.join('、')}</em> : null}{task.acceptance_criteria.length ? <em>验收：{task.acceptance_criteria.join('；')}</em> : null}{task.required_checks.length ? <em>宿主检查：{task.required_checks.map(check => <code key={check}>{check}</code>)}</em> : null}</div></div>)}</div> : mission.tasks.length ? <div className="mission-task-list">{mission.tasks.map(task => <div className={`mission-task-row task-${task.status}`} key={task.spec.id}><span className="mission-task-status">{MISSION_TASK_STATUS_LABELS[task.status]}</span><div><strong>{task.spec.title}</strong><small>{task.spec.execution.role_id || '未指定角色'} · {task.spec.execution.executor}{task.spec.execution.model ? ` · ${task.spec.execution.model}` : ''}</small>{task.spec.depends_on.length ? <em>依赖：{task.spec.depends_on.map(id => taskTitle(mission, id)).join('、')}</em> : null}{task.status === 'failed' ? <em>验收未通过，等待跟进任务</em> : null}</div></div>)}</div> : <p className="muted">确认计划后会在这里出现实际任务；空计划不会派发 Agent。</p>}{mission.status === 'active' && mission.tasks.some(task => task.status === 'failed') ? <button type="button" className="btn" disabled={followingUp} onClick={() => void generateFollowUp()}>{followingUp ? <BusyIndicator size={14} /> : <Activity size={14} />}{followingUp ? '协调中' : '生成跟进任务'}</button> : null}</section>
      </div>
      {mission.status === 'active' && mission.tasks.some(task => task.status === 'running' || task.status === 'awaiting_verification') ? (
        <div className="mission-recovery-control">
          <p>继续原运行，避免重复执行。</p>
          <button type="button" className="btn" disabled={dispatching} onClick={() => void dispatchNext()}>
            {dispatching ? <BusyIndicator size={14} /> : <Activity size={14} />}
            {dispatching ? '检查中' : mission.tasks.some(task => task.status === 'awaiting_verification') ? '继续独立验收' : '恢复/检查运行'}
          </button>
        </div>
      ) : null}
      {mission.status === 'awaiting_assignment' ? <CollaborationAssignmentPanel mission={mission} onAssigned={next => { setMission(next); void poll(true); void onRefresh(); }} /> : null}
      {mission.status === 'awaiting_confirmation' ? <CollaborationAuthorizationPanel
        mission={mission}
        selectedTaskIds={selectedTaskIds}
        approvedReadPaths={approvedAbsoluteReadPaths}
        approvedWritePaths={approvedAbsoluteWritePaths}
        onToggleReadPath={path => setApprovedAbsoluteReadPaths(current => togglePath(current, path))}
        onToggleWritePath={path => setApprovedAbsoluteWritePaths(current => togglePath(current, path))}
      /> : null}
      <section className="mission-detail-panel mission-timeline-panel"><div className="mission-detail-panel-head"><div><h3>执行过程</h3><small>{progressDisclosure(mission, events)}</small></div><span className="mission-timeline-actions">{mission.status === 'active' && mission.tasks.some(task => task.status === 'ready') ? <button type="button" className="btn btn-primary" disabled={dispatching} onClick={() => void dispatchNext()}>{dispatching ? <BusyIndicator size={14} /> : <Plus size={14} />}{dispatching ? '执行中' : '派发下一个任务'}</button> : null}<button type="button" className="btn" onClick={() => { setBusy(true); void poll(true).finally(() => setBusy(false)); }}>{busy ? <BusyIndicator size={14} /> : <Clock3 size={14} />}刷新</button></span></div>{events.length ? <ol className="mission-timeline">{events.map(event => <li key={event.event_id}><span className={`mission-timeline-dot type-${event.event_type}`} aria-hidden="true" /><div><strong>{eventLabel(event)}{event.task_id ? ` · ${taskTitle(mission, event.task_id)}` : ''}</strong><p>{event.summary}</p><time dateTime={event.occurred_at}>{new Date(event.occurred_at).toLocaleString()}</time></div></li>)}</ol> : <p className="muted">暂无细粒度执行事件，只显示任务状态。</p>}</section>
      {mission.status === 'completed' ? <CollaborationWorkflowExportPanel mission={mission} /> : null}
      <div className="mission-detail-foot"><span>确认哈希 {mission.plan_sha256 ? `${mission.plan_sha256.slice(0, 12)}…` : '尚未生成'}</span><span>{mission.budget ? `预算 ${mission.budget.max_rounds} 轮 / ${mission.budget.max_minutes} 分钟 / ${mission.budget.max_repair_attempts} 次返工${mission.budget.max_total_tokens ? ` / ${mission.budget.max_total_tokens.toLocaleString()} Token` : ''}${mission.budget.max_cost ? ` / ${mission.budget.max_cost} 成本` : ''}${mission.usage ? ` · 已用 ${(mission.usage.input_tokens + mission.usage.output_tokens).toLocaleString()} Token${mission.usage.estimated_cost > 0 ? ` / ${mission.usage.estimated_cost}` : ''}` : ''}` : '预算待计划生成'}</span></div>
    </section>
  );
}

function mergeEvents(current: MissionEvent[], next: MissionEvent[]) {
  const byId = new Map(current.map(event => [event.event_id, event]));
  for (const event of next) byId.set(event.event_id, event);
  return [...byId.values()].sort((left, right) => left.sequence - right.sequence);
}

function CollaborationAssignmentPanel({ mission, onAssigned }: { mission: MissionView; onAssigned: (mission: MissionView) => void }) {
  const [options, setOptions] = useState<MissionAssignmentOptions | null>(null);
  const [roles, setRoles] = useState<MissionRoleAssignment[]>([]);
  const [taskRoles, setTaskRoles] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [recommended, setRecommended] = useState(true);

  useEffect(() => {
    let active = true;
    void agentApi.missionAssignmentOptions(mission.mission_id).then(next => {
      if (!active) return;
      setOptions(next);
      setRoles(next.roles.map(role => ({
        role_id: role.role_id, label: role.label, executor: role.recommended_executor,
        service_id: role.recommended_service_id, model: role.recommended_model,
      })));
      const fallback = next.roles.find(role => role.availability === 'available')?.role_id || next.roles[0]?.role_id || '';
      setTaskRoles(Object.fromEntries(mission.proposed_tasks.map(task => [task.id, task.execution.role_id || fallback])));
    }).catch(cause => setError(formatError(cause, '指派选项读取失败')));
    return () => { active = false; };
  }, [mission.mission_id]);

  function useRecommended() {
    if (!options) return;
    setRecommended(true);
    setRoles(options.roles.map(role => ({
      role_id: role.role_id, label: role.label, executor: role.recommended_executor,
      service_id: role.recommended_service_id, model: role.recommended_model,
    })));
  }

  async function submit() {
    if (!options || busy) return;
    setBusy(true); setError('');
    try {
      const input: MissionAssignmentInput = {
        roles: roles.map(role => ({ ...role, service_id: role.service_id.trim(), model: role.model.trim() })),
        task_roles: mission.proposed_tasks.map(task => ({ task_id: task.id, role_id: taskRoles[task.id] || '' })),
        applied_by: '本机用户', recommended,
      };
      onAssigned(await agentApi.assignMissionRoles(mission.mission_id, input));
    } catch (cause) { setError(formatError(cause, '角色指派失败')); }
    finally { setBusy(false); }
  }

  return (
    <section className="mission-detail-panel collaboration-assignment-panel" aria-labelledby="mission-assignment-title">
      <div className="mission-detail-panel-head">
        <div><h3 id="mission-assignment-title">确认角色与服务</h3><small>{options ? `${options.workflow_id}@${options.workflow_version}` : '正在读取固定工作流'}</small></div>
        <ShieldCheck size={16} aria-hidden="true" />
      </div>
      <p>这一步只应用执行配置。计划确认和文件访问授权仍需单独完成。</p>
      {error ? <p className="error" role="alert">{error}</p> : null}
      {options ? <div className="collaboration-assignment-list">{options.roles.map(option => {
        const value = roles.find(role => role.role_id === option.role_id);
        if (!value) return null;
        return <article className="collaboration-assignment-role" key={option.role_id}>
          <div><strong>{option.label || option.role_id}</strong><small>{option.brief || option.role_id}</small><em className={`collaboration-assignment-availability is-${option.availability}`}>{option.availability === 'available' ? '可用' : option.unavailable_reason || '不可用'}</em></div>
          <label><span>执行器</span><input value={value.executor} onChange={event => { setRecommended(false); setRoles(current => current.map(role => role.role_id === option.role_id ? { ...role, executor: event.target.value, model: event.target.value === 'himind.builtin' ? role.model : '' } : role)); }} /></label>
          <label><span>服务</span><input value={value.service_id} onChange={event => { setRecommended(false); setRoles(current => current.map(role => role.role_id === option.role_id ? { ...role, service_id: event.target.value } : role)); }} /></label>
          <label><span>模型</span><input value={value.model} disabled={!option.model_override_supported} onChange={event => { setRecommended(false); setRoles(current => current.map(role => role.role_id === option.role_id ? { ...role, model: event.target.value } : role)); }} /><small>{option.model_override_supported ? '留空表示跟随当前服务默认模型' : '该执行器使用客户端自身模型配置'}</small></label>
        </article>;
      })}</div> : <div className="collaboration-task-loading"><BusyIndicator size={16} />正在读取角色</div>}
      {mission.proposed_tasks.length ? <div className="collaboration-task-role-map"><strong>任务角色映射</strong>{mission.proposed_tasks.map(task => <label key={task.id}><span>{task.title}</span><select value={taskRoles[task.id] || ''} onChange={event => { setRecommended(false); setTaskRoles(current => ({ ...current, [task.id]: event.target.value })); }}>{options?.roles.map(role => <option key={role.role_id} value={role.role_id}>{role.label || role.role_id}</option>)}</select></label>)}</div> : null}
      <div className="collaboration-assignment-actions"><button type="button" className="btn" disabled={!options || busy} onClick={useRecommended}>使用推荐服务与模型</button><button type="button" className="btn btn-primary" disabled={!options || busy || roles.length === 0} onClick={() => void submit()}>{busy ? <BusyIndicator size={14} /> : <ShieldCheck size={14} />}{busy ? '应用中' : '应用角色指派'}</button></div>
    </section>
  );
}

function CollaborationAuthorizationPanel({ mission, selectedTaskIds, approvedReadPaths, approvedWritePaths, onToggleReadPath, onToggleWritePath }: {
  mission: MissionView;
  selectedTaskIds: string[];
  approvedReadPaths: string[];
  approvedWritePaths: string[];
  onToggleReadPath: (path: string) => void;
  onToggleWritePath: (path: string) => void;
}) {
  const choices = collectAbsolutePathChoices(mission.proposed_tasks);
  if (!choices.length) return null;

  const includedTasks = eligibleMissionTasks(mission.proposed_tasks, approvedReadPaths, approvedWritePaths, selectedTaskIds);
  const includedIds = new Set(includedTasks.map(task => task.id));
  const excludedTasks = mission.proposed_tasks.filter(task => !includedIds.has(task.id));

  return (
    <section className="mission-detail-panel mission-path-authorization" aria-labelledby="mission-path-authorization-title">
      <div className="mission-detail-panel-head">
        <div>
          <h3 id="mission-path-authorization-title">绝对路径授权</h3>
          <small>{includedTasks.length}/{mission.proposed_tasks.length} 项任务将纳入确认</small>
        </div>
        <ShieldCheck size={16} aria-hidden="true" />
      </div>
      <p>先选允许访问的路径；未授权任务不会派发。</p>
      <div className="mission-path-choice-list">
        {choices.map(choice => (
          <article className="mission-path-choice" key={choice.key}>
            <div className="mission-path-choice-info">
              <code title={choice.path}>{choice.path}</code>
              <small>{isPathWithinWorkspace(choice.path, mission.workspace.root) ? '工作区内绝对路径' : '工作区外绝对路径 · 需扩展授权'}</small>
              <small>读取：{choice.readTasks.map(task => task.title).join('、') || '无'}；写入：{choice.writeTasks.map(task => task.title).join('、') || '无'}</small>
            </div>
            <div className="mission-path-permissions">
              {choice.readTasks.length ? (
                <label>
                  <input type="checkbox" aria-label={`允许读取绝对路径 ${choice.path}`} checked={hasPath(approvedReadPaths, choice.path)} onChange={() => onToggleReadPath(choice.path)} />
                  <span>允许读取</span>
                </label>
              ) : null}
              {choice.writeTasks.length ? (
                <label>
                  <input type="checkbox" aria-label={`允许写入绝对路径 ${choice.path}`} checked={hasPath(approvedWritePaths, choice.path)} onChange={() => onToggleWritePath(choice.path)} />
                  <span>允许写入</span>
                </label>
              ) : null}
            </div>
          </article>
        ))}
      </div>
      {excludedTasks.length ? (
        <div className="mission-path-excluded" role="status">
          <strong>不会派发的任务</strong>
          <ul>{excludedTasks.map(task => <li key={task.id}><span>{task.title}</span><small>{missionTaskExclusionReason(task, includedIds, approvedReadPaths, approvedWritePaths, mission.proposed_tasks)}</small></li>)}</ul>
        </div>
      ) : <p className="mission-path-ready" role="status">当前计划任务所需的绝对路径均已授权。</p>}
    </section>
  );
}

function collectAbsolutePathChoices(tasks: MissionTaskSpec[]) {
  const choices = new Map<string, { key: string; path: string; readTasks: MissionTaskSpec[]; writeTasks: MissionTaskSpec[] }>();
  for (const task of tasks) {
    for (const [paths, kind] of [[task.read_paths, 'read'], [task.write_paths, 'write']] as const) {
      for (const path of paths.filter(isAbsoluteMissionPath)) {
        const key = missionPathKey(path);
        const choice = choices.get(key) ?? { key, path, readTasks: [], writeTasks: [] };
        const relatedTasks = kind === 'read' ? choice.readTasks : choice.writeTasks;
        if (!relatedTasks.some(item => item.id === task.id)) relatedTasks.push(task);
        choices.set(key, choice);
      }
    }
  }
  return [...choices.values()].sort((left, right) => left.path.localeCompare(right.path));
}

function eligibleMissionTasks(tasks: MissionTaskSpec[], approvedReadPaths: string[], approvedWritePaths: string[], selectedTaskIds?: string[]) {
  const selected = new Set(tasks
    .filter(task => (!selectedTaskIds || selectedTaskIds.includes(task.id)) && taskUsesAuthorizedAbsolutePaths(task, approvedReadPaths, approvedWritePaths))
    .map(task => task.id));
  let changed = true;
  while (changed) {
    changed = false;
    for (const task of tasks) {
      if (selected.has(task.id) && task.depends_on.some(dependency => !selected.has(dependency))) {
        selected.delete(task.id);
        changed = true;
      }
    }
  }
  return tasks.filter(task => selected.has(task.id));
}

function taskUsesAuthorizedAbsolutePaths(task: MissionTaskSpec, approvedReadPaths: string[], approvedWritePaths: string[]) {
  return task.read_paths.filter(isAbsoluteMissionPath).every(path => hasPath(approvedReadPaths, path))
    && task.write_paths.filter(isAbsoluteMissionPath).every(path => hasPath(approvedWritePaths, path));
}

function CollaborationWorkflowExportPanel({ mission }: { mission: MissionView }) {
  const [workflowId, setWorkflowId] = useState(`com.himind.workflow.${mission.mission_id.replace(/[^a-zA-Z0-9._-]/g, '-')}`);
  const [version, setVersion] = useState('1.0.0');
  const [name, setName] = useState(`${mission.title} 协作流程`);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  async function exportWorkflow() {
    if (busy) return;
    setBusy(true); setError(''); setNotice('');
    try {
      const draft = await agentApi.exportMissionWorkflow(mission.mission_id, workflowId.trim(), version.trim(), name.trim(), 'start');
      setNotice(`候选 ${draft.package_id}@${draft.version} 已生成 · ${draft.state}。请到开发工作区确认。`);
    } catch (cause) { setError(formatError(cause, '协作工作流导出失败')); }
    finally { setBusy(false); }
  }
  return <section className="mission-detail-panel collaboration-export-panel" aria-labelledby="mission-export-title">
    <div className="mission-detail-panel-head"><div><h3 id="mission-export-title">沉淀为协作工作流</h3><small>只提取已验证的角色、步骤与验收；不会发布</small></div><Download size={16} aria-hidden="true" /></div>
    <p>仅保留可复用角色、步骤和验收。</p>
    <p>本次路径、对话、运行记录与用量不会导出。</p>
    <p>导出只生成候选并预检，不提交审核。</p>
    {error ? <p className="error" role="alert">{error}</p> : null}
    {notice ? <p className="collaboration-export-notice" role="status">{notice}</p> : null}
    <div className="collaboration-export-fields">
      <label><span>工作流 ID</span><input value={workflowId} onChange={event => setWorkflowId(event.target.value)} /></label>
      <label><span>版本</span><input value={version} onChange={event => setVersion(event.target.value)} /></label>
      <label><span>名称</span><input value={name} onChange={event => setName(event.target.value)} /></label>
      <button type="button" className="btn" disabled={busy || !workflowId.trim() || !version.trim() || !name.trim()} onClick={() => void exportWorkflow()}>{busy ? <BusyIndicator size={14} /> : <Download size={14} />}{busy ? '生成中' : '生成工作流候选'}</button>
    </div>
  </section>;
}

function missionTaskExclusionReason(task: MissionTaskSpec, includedIds: Set<string>, approvedReadPaths: string[], approvedWritePaths: string[], tasks: MissionTaskSpec[]) {
  const deniedRead = task.read_paths.find(path => isAbsoluteMissionPath(path) && !hasPath(approvedReadPaths, path));
  if (deniedRead) return `未授权读取：${deniedRead}`;
  const deniedWrite = task.write_paths.find(path => isAbsoluteMissionPath(path) && !hasPath(approvedWritePaths, path));
  if (deniedWrite) return `未授权写入：${deniedWrite}`;
  const missingDependency = task.depends_on.find(id => !includedIds.has(id));
  return missingDependency ? `依赖任务未纳入：${tasks.find(item => item.id === missingDependency)?.title || missingDependency}` : '未纳入本次计划';
}

function isAbsoluteMissionPath(path: string) {
  const normalized = path.trim().replace(/\\/g, '/');
  return /^[A-Za-z]:\//.test(normalized);
}

function missionPathKey(path: string) {
  return path.trim().replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();
}

function hasPath(paths: string[], path: string) {
  const key = missionPathKey(path);
  return paths.some(item => missionPathKey(item) === key);
}

function togglePath(paths: string[], path: string) {
  return hasPath(paths, path) ? paths.filter(item => missionPathKey(item) !== missionPathKey(path)) : [...paths, path];
}

function isPathWithinWorkspace(path: string, root: string) {
  const target = missionPathKey(path);
  const base = missionPathKey(root);
  return target === base || target.startsWith(`${base}/`);
}

function toggleMissionTaskSelection(tasks: MissionTaskSpec[], current: string[], taskId: string) {
  const selected = new Set(current);
  if (selected.has(taskId)) {
    selected.delete(taskId);
    let changed = true;
    while (changed) {
      changed = false;
      for (const task of tasks) {
        if (selected.has(task.id) && task.depends_on.some(id => !selected.has(id))) {
          selected.delete(task.id);
          changed = true;
        }
      }
    }
  } else {
    const include = (id: string) => {
      if (selected.has(id)) return;
      selected.add(id);
      tasks.find(task => task.id === id)?.depends_on.forEach(include);
    };
    include(taskId);
  }
  return tasks.filter(task => selected.has(task.id)).map(task => task.id);
}
