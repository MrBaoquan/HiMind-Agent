import { useCallback, useEffect, useRef, useState } from 'react';
import { Activity, CheckCircle2, Clock3, Download, Plus, ShieldCheck } from 'lucide-react';
import { BusyIndicator } from '../components/BusyIndicator';
import { EmptyState } from '../components/Common';
import { agentApi, type AgentActivityItem, type MissionAssignmentInput, type MissionAssignmentOptions, type WorkflowCenterItem, type MissionBudgetIncrease, type MissionEvent, type MissionExecutionOption, type MissionRoleAssignment, type MissionSnapshot, type MissionTaskSpec, type MissionView } from '../services/agentApi';
import { formatError } from '../types';
import { formatRelativeStamp, useNowTick } from '../timeFormat';
import { ENDED_STATUSES, executionTodoId, missionTodo, clientLabel, activityStatusLabel } from './taskCenterModel';
import { taskStatusLabel } from './taskView';
import { MissionEvidence, MissionTaskEvidence } from './MissionEvidence';
import { MissionBudgetSummary } from './MissionBudgetSummary';
import { readMissionSnapshot } from './missionSnapshot';
import { pendingBudgetRequest, retainBudgetRequest, clearBudgetRequest } from './missionBudgetRequest';
import { RuntimeWorkspaceDialog } from './RuntimeWorkspaceDialog';
import { CollaborationDialog } from './CollaborationDialog';
import { assignmentProblem, bindExpert, changeExecutor, changeService, expertKey, serviceOptions } from './missionAssignment';
import {
  eventLabel,
  canRetryWaitingVerification,
  legacyBudgetExtensionAction,
  legacyVerificationOverrideForTask,
  missionNextAction,
  missionRunningVerifications,
  missionWait,
  missionResponsibility,
  missionResumeAction,
  missionActionActor,
  missionRuntimeBlocked,
  missionProgress,
  missionFlow,
  missionStatusLabel,
  missionStage,
  missionVerificationExplanation,
  missionStalled,
  taskFailureReason,
  MISSION_STAGE_LABELS,
  MISSION_TASK_STATUS_LABELS,
  progressDisclosure,
  taskTitle,
} from './missionView';

const COLLAB_ACTIVE_STATUSES = new Set(['draft', 'awaiting_assignment', 'planned', 'awaiting_confirmation', 'active', 'waiting']);

function MissionPermissionPanel({ mission, events, onSaved }: { mission: MissionView; events: MissionEvent[]; onSaved: (mission: MissionView) => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const editable = ['draft','awaiting_assignment','planned','awaiting_confirmation'].includes(mission.status);
  const recovery = [...events].reverse().find(event => event.event_type === 'runtime_workspace_resolved' && event.payload?.plan_sha256 === mission.plan_sha256);
  const full = recovery ? recovery.payload?.mode === 'full_access' : mission.workspace.permission_mode === 'full_access';
  async function save(mode: 'workspace_write' | 'full_access') {
    setBusy(true); setError('');
    try { onSaved(await agentApi.setMissionPermissionMode(mission.mission_id, mission.plan_sha256, mode)); }
    catch (cause) { setError(formatError(cause, '执行权限设置失败')); }
    finally { setBusy(false); }
  }
  return <section className="mission-detail-panel"><h3>执行权限</h3>
    {editable ? <label><span>任务访问模式</span><select value={full ? 'full_access' : 'workspace_write'} disabled={busy} onChange={event => void save(event.target.value as 'workspace_write' | 'full_access')}><option value="workspace_write">工作区访问</option><option value="full_access">完全访问</option></select></label> : <strong>{full ? '完全访问' : recovery?.payload?.mode === 'files_only' ? '仅文件工具' : '工作区访问'}</strong>}
    <p>{full ? '按当前用户权限访问工作区外文件；ACP 请求自动批准。目标与验收标准仍有效。' : 'HiMind AI 使用工作区沙箱；外部执行器遵循自身访问控制。受阻时可调整权限。'}</p>
    {editable ? <small>随计划确认后生效，后续执行无需重复确认；不会自动提权为管理员。</small> : null}
    {error ? <p role="alert" className="error">{error}</p> : null}
  </section>;
}

export function CreateMissionDialog({ onClose, onCreated }: { onClose: () => void; onCreated: (mission: MissionView) => void }) {
  const [title, setTitle] = useState('');
  const [objective, setObjective] = useState('');
  const [root, setRoot] = useState('');
  const [writeScope, setWriteScope] = useState('');
  const [permissionMode, setPermissionMode] = useState<'workspace_write' | 'full_access'>('workspace_write');
  const [acceptance, setAcceptance] = useState('');
  const [workflows, setWorkflows] = useState<WorkflowCenterItem[]>([]);
  const [workflowId, setWorkflowId] = useState('');
  const requestId = useRef(`agent-ui:${crypto.randomUUID()}`);
  useEffect(() => {
    void agentApi.workflowCenter().then(snapshot => setWorkflows(snapshot.workflows.filter(item => item.enabled && !item.development && (Object.keys(item.package.roles || {}).length > 0 || item.package.steps.some(step => step.role_id?.trim())))))
      .catch(cause => setError(formatError(cause, '协作工作流读取失败')));
  }, []);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  async function submit() {
    if (busy) return;
    setBusy(true);
    setError('');
    try {
      const selected = workflows.find(item => item.package.id === workflowId);
      if (!selected) throw new Error('请选择已安装的协作工作流');
      onCreated(await agentApi.createMissionHandoff({
        idempotency_key: requestId.current, source_client: 'agent-ui', source_reference: requestId.current,
        title: title.trim(), objective: objective.trim(),
        workspace: { root: root.trim(), permission_mode: permissionMode, read_scope: ['.'], write_scope: writeScope.split(/[,\n]/).map(value => value.trim()).filter(Boolean) },
        acceptance_criteria: acceptance.split('\n').map(value => value.trim()).filter(Boolean),
        workflow: { workflow_id: selected.package.id, workflow_version: selected.package.version, workflow_sha256: selected.package_digest, entrypoint: selected.package.default_entrypoint || '' },
        plan_mode: 'generated',
      }));
    } catch (cause) {
      setError(formatError(cause, '协作任务创建失败'));
    } finally {
      setBusy(false);
    }
  }

  return (
    <CollaborationDialog titleId="mission-create-title" onClose={onClose}>
      <section className="collaboration-create-dialog">
        <header><div><span>新建协作任务</span><h2 id="mission-create-title">创建协作任务</h2></div><button type="button" className="btn" onClick={onClose}>关闭</button></header>
        <div className="collaboration-create-fields">
          <label><span>协作工作流 *</span><select value={workflowId} onChange={event => setWorkflowId(event.target.value)}><option value="">请选择已安装工作流</option>{workflows.map(item => <option key={item.package.id} value={item.package.id}>{item.package.name} · {item.package.version}</option>)}</select></label>
          {!workflows.length ? <p className="muted">请先在“我的能力”安装协作工作流。</p> : null}
          <label><span>任务标题 *</span><input value={title} onChange={event => setTitle(event.target.value)} placeholder="例如：修复设置页焦点丢失" /></label>
          <label><span>目标</span><textarea value={objective} onChange={event => setObjective(event.target.value)} placeholder="完成后应该看到什么结果？" /></label>
          <label><span>工作区根目录 *</span><input value={root} onChange={event => setRoot(event.target.value)} placeholder="项目的本地绝对路径" /></label>
          <label><span>执行权限</span><select value={permissionMode} onChange={event => setPermissionMode(event.target.value as 'workspace_write' | 'full_access')}><option value="workspace_write">工作区访问</option><option value="full_access">完全访问</option></select></label>
          <small>{permissionMode === 'full_access' ? '确认计划后，HiMind AI 或 ACP 执行器按当前系统用户权限运行，ACP 工具请求自动批准。不自动提权为管理员。' : 'HiMind AI 使用工作区沙箱；外部执行器遵循自身的访问控制。'}</small>
          <label><span>允许修改的范围 *</span><textarea value={writeScope} onChange={event => setWriteScope(event.target.value)} placeholder="例如：frontend/src，行或逗号分隔" /></label>
          <label><span>完成标准</span><textarea value={acceptance} onChange={event => setAcceptance(event.target.value)} placeholder="每行一条可验证标准" /></label>
        </div>
        {error ? <p className="error" role="alert">{error}</p> : null}
        <footer><small>创建后选择专家，再确认计划。</small><button type="button" className="btn btn-primary" disabled={busy || !workflowId || !title.trim() || !root.trim()} onClick={() => void submit()}>{busy ? <BusyIndicator size={14} /> : <Plus size={14} />}{busy ? '创建中' : '创建任务'}</button></footer>
      </section>
    </CollaborationDialog>
  );
}

function TaskExecutionEditor({ mission, task, onSaved }: {
  mission: MissionView; task: MissionTaskSpec; onSaved: (mission: MissionView) => void;
}) {
  const [roleId, setRoleId] = useState(task.execution.role_id);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const roles = mission.assignment?.roles || [];
  useEffect(() => { setRoleId(task.execution.role_id); }, [task.execution.role_id]);
  if (!roles.length) return <small className="muted">历史任务保留原执行绑定；新任务请先指派专家。</small>;
  return <div className="mission-execution-editor">
    <label><span>承担专家</span><select value={roleId} onChange={event => setRoleId(event.target.value)}>{roles.map(role => <option key={role.role_id} value={role.role_id}>{role.label || role.role_id}</option>)}</select></label>
    <button type="button" className="btn" disabled={saving || !roleId || roleId === task.execution.role_id} onClick={async () => {
      const role = roles.find(item => item.role_id === roleId); if (!role) return;
      setSaving(true); setError('');
      try { onSaved(await agentApi.updateMissionTaskExecution(mission.mission_id, task.id, mission.plan_sha256, { role_id: role.role_id, executor: role.executor, service_id: role.service_id, model: role.model, expert: role.expert })); }
      catch (cause) { setError(formatError(cause, '任务指派更新失败')); } finally { setSaving(false); }
    }}>保存任务指派</button>
    {error ? <small className="error">{error}</small> : null}
  </div>;
}

export function CollaborationTaskDetail({ missionId, openAssignmentRevision = 0, onBack, onRefresh, onMissionChanged, relatedActivities = [], onOpenWorkflowRun }: {
  missionId: string; openAssignmentRevision?: number; onBack: () => void; onRefresh: () => Promise<void>;
  onMissionChanged?: (mission: MissionView) => void; relatedActivities?: AgentActivityItem[];
  onOpenWorkflowRun?: (runId: string) => void;
}) {
  const [tab, setTab] = useState<'overview' | 'plan' | 'experts' | 'activity'>('overview');
  const [mission, setMission] = useState<MissionView | null>(null);
  const [events, setEvents] = useState<MissionEvent[]>([]);
  const [historyComplete, setHistoryComplete] = useState(false);
  const cursorRef = useRef(0);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [notice, setNotice] = useState('');
  const [dispatching, setDispatching] = useState(false);
  const [planning, setPlanning] = useState(false);
  const [followingUp, setFollowingUp] = useState(false);
  const [rechecking, setRechecking] = useState('');
  const [controlling, setControlling] = useState(false);
  const [runtimeWorkspaceOpen, setRuntimeWorkspaceOpen] = useState(false);
  const [budgetExtensionOpen, setBudgetExtensionOpen] = useState(false);
  const [assignmentOpen, setAssignmentOpen] = useState(false);
  const openedAssignment = useRef('');
  useEffect(() => {
    const request = `${missionId}:${openAssignmentRevision}`;
    if (mission?.status === 'awaiting_assignment' && openedAssignment.current !== request) {
      openedAssignment.current = request; setAssignmentOpen(true);
    }
  }, [missionId, mission?.status, openAssignmentRevision]);
  const [selectedTaskIds, setSelectedTaskIds] = useState<string[]>([]);
  const [approvedAbsoluteReadPaths, setApprovedAbsoluteReadPaths] = useState<string[]>([]);
  const [approvedAbsoluteWritePaths, setApprovedAbsoluteWritePaths] = useState<string[]>([]);
  const reviewPlanRef = useRef('');

  const pollRevision = useRef(0);
  const poll = useCallback(async (reset = false) => {
    const revision = ++pollRevision.current;
    try {
      const head = cursorRef.current;
      const loaded = await readMissionSnapshot(after => agentApi.missionSnapshot(missionId, after), reset ? 0 : head);
      let snapshot: MissionSnapshot = loaded.snapshot;
      reset = loaded.replace;
      if (revision !== pollRevision.current || snapshot.mission.mission_id !== missionId
          || !Number.isSafeInteger(snapshot.cursor) || snapshot.cursor < head) return;
      const mismatched = Boolean(snapshot.mission.effective_wait && snapshot.mission.effective_wait.cursor !== snapshot.cursor);
      if (mismatched) snapshot = {...snapshot, mission: {...snapshot.mission, effective_wait: null}};
      if (snapshot.mission.status === 'awaiting_confirmation' && snapshot.mission.plan_sha256 && reviewPlanRef.current !== snapshot.mission.plan_sha256) {
        reviewPlanRef.current = snapshot.mission.plan_sha256;
        setSelectedTaskIds(snapshot.mission.proposed_tasks.map(task => task.id));
        setApprovedAbsoluteReadPaths([]);
        setApprovedAbsoluteWritePaths([]);
      }
      setMission(snapshot.mission);
      onMissionChanged?.(snapshot.mission);
      setEvents(current => reset ? snapshot.events : mergeEvents(current, snapshot.events));
      setHistoryComplete(current => (reset || current) && !snapshot.requires_snapshot && !mismatched && snapshot.mission.effective_wait?.cursor === snapshot.cursor);
      cursorRef.current = snapshot.cursor;
      setError(mismatched ? '状态快照关联不一致，增额与恢复资格暂未确认；请刷新。' : '');
    } catch (cause) {
      if (revision !== pollRevision.current) return;
      setMission(current => current ? {...current, effective_wait: null} : current);
      setHistoryComplete(false);
      setError(formatError(cause, '协作任务详情读取失败'));
    }
  }, [missionId, onMissionChanged]);



  useEffect(() => {
    cursorRef.current = 0; setMission(null); setEvents([]); setHistoryComplete(false); void poll(true);
    const timer = window.setInterval(() => {
      if (document.visibilityState !== 'hidden') void poll();
    }, 5000);
    return () => { ++pollRevision.current; window.clearInterval(timer); };
  }, [missionId, poll]);

  // Store budget time is wall time since confirmation, including every wait.
  // There is no contractual terminal timestamp: never freeze at updated_at.
  const now = useNowTick(Boolean(mission?.confirmation));
  if (!mission) return <section className="mission-detail"><button type="button" className="btn" onClick={onBack}>返回我的任务</button>{error ? <p className="error" role="alert">{error}</p> : <div className="collaboration-task-loading"><BusyIndicator size={18} />正在读取任务详情</div>}</section>;
  const stage = missionStage(mission);
  const progress = missionProgress(mission);
  const flow = missionFlow(mission);
  const verificationExplanation = missionVerificationExplanation(mission, events, relatedActivities);
  const currentFlowIndex = flow.findIndex(step => step.state === 'current');
  const currentStageIsVerification = currentFlowIndex === 3;
  const runningVerifications = missionRunningVerifications(mission, relatedActivities);
  const verificationRunLive = runningVerifications.length > 0;
  const verificationStarted = progress.awaitingVerification > 0 && progress.startedVerification > 0;
  const verificationQueued = mission.status === 'active' && progress.awaitingVerification > 0 && !verificationStarted;
  const liveWork = verificationRunLive || (mission.status === 'active' && (progress.running > 0 || planning || confirming || dispatching));
  const detailStatusLabel = missionStatusLabel(mission, events, relatedActivities);
  const runtimeBlocked = missionRuntimeBlocked(mission, events);
  const wait = missionWait(mission);
  const resumeAction = missionResumeAction(mission, relatedActivities);
  const pendingBudget = pendingBudgetRequest(missionId, mission.plan_sha256);
  const canRevisit = mission.status === 'active' && !mission.tasks.some(task => ['running','ready','awaiting_verification'].includes(task.status))
    && mission.tasks.some(task => task.status === 'failed');
  const detailStageLabel = runtimeBlocked ? '命令环境待处理' : mission.status === 'waiting' && currentFlowIndex >= 0
    ? flow[currentFlowIndex].label
    : MISSION_STAGE_LABELS[stage];

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

  async function recheckTask(taskId: string, runId: string, legacyOverride?: { expected_pause_event_id: string; expected_runtime_failure_event_id: string } | null) {
    if (!mission || rechecking) return;
    setRechecking(taskId);
    setError('');
    try {
      await agentApi.retryMissionVerification(missionId, taskId, {
        request_id: `agent-ui:recheck:${taskId}:${runId}`,
        expected_plan_sha256: mission.plan_sha256, expected_verification_run_id: runId,
        ...(legacyOverride ? { legacy_verification_override: legacyOverride } : {}),
      });
      await poll(true);
      await onRefresh();
    } catch (cause) { setError(formatError(cause, '重新核验失败')); }
    finally { setRechecking(''); }
  }

  const todo = missionTodo(mission, relatedActivities);
  const userRuns = relatedActivities.filter(item=>!ENDED_STATUSES.has(mission.status) && item.status==='waiting' && ['approval','feedback','form','evidence'].includes(item.pause_kind||''));
  const budgetExhausted = mission.status === 'waiting' && wait?.reason === 'budget_exhausted';
  const legacyCandidates = mission.tasks.map(task => ({ task, override: legacyVerificationOverrideForTask(mission, events, task) }))
    .filter(item => item.override !== null);
  const legacyRetry = legacyCandidates.find(item => canRetryWaitingVerification(mission, events, item.task));
  return (
    <section className="mission-detail" data-mission-id={mission.mission_id} data-todo-id={todo?.id} data-snapshot-cursor={wait?.cursor} data-wait-reason={wait?.reason} data-wait-actor={missionActionActor(mission, relatedActivities)}>
      <div className="mission-detail-head"><button type="button" className="btn" onClick={onBack}>返回我的任务</button><span className={'collaboration-task-status status-'+mission.status}>{detailStatusLabel}</span></div>
      <header className="mission-detail-hero"><div><span className="collaboration-task-kicker"><ShieldCheck size={15}/>协作任务</span><h2>{mission.title}</h2><p>{mission.objective || '尚未填写目标说明'}</p></div>
        {progress.total ? <div className="mission-detail-progress"><strong>{progress.done}/{progress.total}</strong><span>原始交付已通过验收</span><div className="task-center-progress"><span style={{width:Math.round(progress.done/progress.total*100)+'%'}}/></div><small>{progress.running} 执行中 · {progress.awaitingVerification} 项结果待核验</small></div> : <span className="muted">未记录执行进度</span>}
      </header>
      {!['failed', 'canceled'].includes(mission.status) ? <nav className={`mission-stage-rail${liveWork ? ' is-live' : ''}`} aria-label="协作任务进度">
        {flow.map((step, index) => <div key={step.label} className={`is-${step.state}${step.state === 'current' && currentStageIsVerification && !verificationStarted ? ' is-attention' : ''}${step.state === 'current' && liveWork ? ' is-running' : ''}`} aria-current={step.state === 'current' ? 'step' : undefined}>
          <span>{index < currentFlowIndex || mission.status === 'completed' ? <CheckCircle2 size={13} /> : index + 1}</span><strong>{step.label}</strong>
          {step.state === 'current' ? <small>{currentStageIsVerification ? (verificationRunLive ? '另一位专家核验中' : verificationStarted ? '核验任务已派发' : verificationQueued ? '等待系统安排核验' : '等待任务恢复') : liveWork ? '正在处理' : '当前阶段'}</small> : null}
        </div>)}
      </nav> : null}
      <dl className="task-facts"><div><dt>工作区</dt><dd>{mission.workspace.root || '未记录'}</dd></div><div><dt>来源</dt><dd>{clientLabel(mission.handoff?.source_client || '')}</dd></div><div><dt>当前阶段</dt><dd>{detailStageLabel}</dd></div><div><dt>更新时间</dt><dd>{new Date(mission.updated_at).toLocaleString()}</dd></div></dl>
      {error ? <p className="error" role="alert">{error}</p> : null}
      {notice ? <p role="status">{notice}</p> : null}
      <div className="mission-current-action" role="status" aria-live="polite" aria-atomic="true">
        <div><strong>{liveWork ? <BusyIndicator size={14}/> : null}{missionStatusLabel(mission, events, relatedActivities)} · {missionNextAction(mission, events, relatedActivities)}</strong><p>{missionResponsibility(mission, relatedActivities)}</p>{verificationExplanation ? <p>{verificationExplanation}</p> : ['waiting','failed'].includes(mission.status) || missionStalled(mission) ? <p>{wait?.budget_reason || (runtimeBlocked ? '命令环境需要处理；授权范围由你选择。' : '以当前快照为准，活动记录仅供追溯。')}</p> : null}{events.length > 0 ? <p className="muted">最近事件：{eventLabel(events[events.length - 1])} · {formatRelativeStamp(events[events.length - 1].occurred_at, now)}。状态每 5 秒刷新。</p> : null}</div>
        {mission.status==='awaiting_assignment' ? <button className="btn btn-primary" onClick={()=>setAssignmentOpen(true)}>选择协作专家</button>
        : ['draft','planned'].includes(mission.status) ? <button className="btn btn-primary" disabled={planning} onClick={()=>void generatePlan()}>{planning?'生成中':'生成执行计划'}</button>
        : mission.status==='awaiting_confirmation' ? <button className="btn btn-primary" onClick={()=>setTab('plan')}>审查计划与访问范围</button>
        : (verificationRunLive || mission.status==='active') && progress.awaitingVerification > 0 ? <button className="btn btn-primary" onClick={()=>setTab('plan')}>查看验收详情</button>
        : missionStalled(mission) ? <button className="btn btn-primary" onClick={()=>setTab('plan')}>查看失败原因</button>
        : runtimeBlocked ? <button className="btn btn-primary" onClick={()=>setRuntimeWorkspaceOpen(true)}>处理命令环境</button>
        : legacyRetry ? <button className="btn btn-primary" disabled={Boolean(rechecking)} onClick={()=>void recheckTask(legacyRetry.task.spec.id, legacyRetry.task.verification_run_id, legacyRetry.override)}>{rechecking === legacyRetry.task.spec.id ? '提交中' : '确认覆盖并重试核验'}</button>
        : mission.status==='waiting' && budgetExhausted && wait?.can_increase_budget ? <button className="btn btn-primary" onClick={()=>setBudgetExtensionOpen(true)}><Plus size={14}/>{legacyCandidates.length ? '补充核验时限' : '增加预算并继续'}</button>
        : resumeAction ? <button className={resumeAction.kind === 'verification' ? 'btn btn-primary' : 'btn'} disabled={controlling} onClick={()=>void controlMission('resume')}>{resumeAction.label}</button>
        : mission.status==='completed' ? <button className="btn" onClick={()=>setTab('overview')}>查看交付结果</button>
        : mission.status==='failed' ? <button className="btn" onClick={()=>setTab('activity')}>查看失败原因</button> : null}
        <details className="mission-more-actions"><summary>任务操作</summary>
          {pendingBudget || wait?.can_increase_budget ? <button className="btn" onClick={()=>setBudgetExtensionOpen(true)}>{pendingBudget ? '重试原增额申请' : '主动增加预算并续跑'}</button> : null}
          <p>续跑继续未完成任务；重核只检查同一实现；重做按方案修改交付。历史结果保留。</p>
          {mission.status==='active'?<button className="btn" disabled={controlling} onClick={()=>void controlMission('pause')}>暂停后续执行</button>:null}
          {COLLAB_ACTIVE_STATUSES.has(mission.status)?<button className="btn" disabled={controlling} onClick={()=>void controlMission('cancel')}>取消任务</button>:null}
          <button className="btn" onClick={()=>{setBusy(true);void poll(true).finally(()=>setBusy(false));}}>{busy?'刷新中':'刷新任务'}</button>
        </details>
      </div>
      {userRuns.length ? <section className="mission-detail-panel"><h3>需要你处理的执行事项</h3>{userRuns.map(item=><div className="task-related-execution" key={item.id} data-todo-id={executionTodoId(item.workflow_run_id || item.id, item.current_step_id || '', item.pause_kind || '')}><span>{taskTitle(mission,item.mission_task_id||'') || item.title} · {activityStatusLabel(item)}</span>{onOpenWorkflowRun&&item.workflow_run_id?<button className="btn btn-primary" onClick={()=>onOpenWorkflowRun(item.workflow_run_id!)}>处理执行事项</button>:null}</div>)}</section>:null}
      <div className="mission-detail-tabs segmented-control" role="tablist" aria-label="任务详情内容">{([{id:'overview',label:'概览'},{id:'plan',label:'执行计划'},{id:'experts',label:'协作专家'},{id:'activity',label:'活动记录'}] as const).map(item=><button key={item.id} role="tab" aria-selected={tab===item.id} className={tab===item.id?'active':''} onClick={()=>setTab(item.id)}>{item.label}</button>)}</div>
      {tab==='overview'?<div role="tabpanel" aria-label="概览">
        <MissionEvidence mission={mission} events={events}/>
        <MissionBudgetSummary mission={mission} events={events} historyComplete={historyComplete} now={now}/>
        <section className="mission-detail-panel"><h3>完成标准</h3>{mission.acceptance_criteria.length?<ul>{mission.acceptance_criteria.map((value,index)=><li key={index}>{value}</li>)}</ul>:<p className="muted">尚未填写完成标准。</p>}{mission.required_acceptance.length?<><h4>必须通过的验收</h4><ul>{mission.required_acceptance.map((value,index)=><li key={index}>{value}</li>)}</ul></>:null}</section>
        <section className="mission-detail-panel"><h3>计划摘要</h3><p>{mission.plan_summary || '尚未生成执行计划。'}</p>{progress.total?<p>{progress.done}/{progress.total} 项已通过验收 · {progress.running} 项执行中 · {progress.awaitingVerification} 项验收未完成</p>:null}<button className="btn" onClick={()=>setTab('plan')}>查看执行计划</button></section>
        {mission.handoff?.context_summary?<section className="mission-detail-panel"><h3>来源上下文</h3><p>{mission.handoff.context_summary}</p></section>:null}
        {mission.status==='completed'?<details className="mission-optional-export"><summary>将本次协作方式保存为工作流</summary><CollaborationWorkflowExportPanel mission={mission}/></details>:null}
      </div>:null}
      {tab==='plan'?<div role="tabpanel" aria-label="执行计划">
      <MissionPermissionPanel mission={mission} events={events} onSaved={next => { setMission(next); void poll(true); void onRefresh(); }} />
      {mission.status === 'awaiting_confirmation' ? <CollaborationAuthorizationPanel
        mission={mission}
        selectedTaskIds={selectedTaskIds}
        approvedReadPaths={approvedAbsoluteReadPaths}
        approvedWritePaths={approvedAbsoluteWritePaths}
        onToggleReadPath={path => setApprovedAbsoluteReadPaths(current => togglePath(current, path))}
        onToggleWritePath={path => setApprovedAbsoluteWritePaths(current => togglePath(current, path))}
      /> : null}
      <div className="mission-detail-grid">
        <section className="mission-detail-panel"><div className="mission-detail-panel-head"><h3>计划与确认</h3><span>r{mission.plan_revision || 0}</span></div>{mission.plan_summary ? <p>{mission.plan_summary}</p> : <p className="muted">还没有执行计划，确认前不会派发写任务。</p>}{(mission.status === 'draft' || mission.status === 'planned') ? <button type="button" className="btn" disabled={planning} onClick={() => void generatePlan()}>{planning ? <BusyIndicator size={14} /> : <ShieldCheck size={14} />}{planning ? '协调中' : '生成执行计划'}</button> : null}{mission.confirmation ? <div className="mission-confirmation"><strong>已确认</strong><small>{mission.confirmation.confirmed_by} · 相对写入：{mission.confirmation.approved_paths.join('、') || '无'} · 绝对读取：{mission.confirmation.approved_absolute_read_paths.join('、') || '无'} · 绝对写入：{mission.confirmation.approved_absolute_write_paths.join('、') || '无'}</small><code title={mission.confirmation.plan_sha256}>{mission.confirmation.plan_sha256.slice(0, 16)}…</code></div> : mission.plan_sha256 ? <div className="mission-confirmation is-pending"><strong>等待确认</strong><small>计划摘要 {mission.plan_sha256.slice(0, 16)}…</small></div> : null}{mission.status === 'awaiting_confirmation' && mission.plan_sha256 ? <button type="button" className="btn btn-primary" disabled={confirming || eligibleMissionTasks(mission.proposed_tasks, approvedAbsoluteReadPaths, approvedAbsoluteWritePaths, selectedTaskIds).length === 0} onClick={() => void confirmPlan()}>{confirming ? <BusyIndicator size={14} /> : <CheckCircle2 size={14} />}{confirming ? '确认中' : '确认计划并允许派发'}</button> : null}</section>
        <section className="mission-detail-panel">
          <div className="mission-detail-panel-head">
            <h3>{mission.status === 'awaiting_confirmation' ? '待确认任务' : '执行子任务'}</h3>
            <span>{(mission.status === 'awaiting_confirmation' ? mission.proposed_tasks : mission.tasks.map(task => task.spec)).length} 项</span>
          </div>
          {mission.status === 'awaiting_confirmation' && mission.proposed_tasks.length ? (
            <div className="mission-task-list">
              {mission.proposed_tasks.map(task => (
                <div className="mission-task-row task-proposed" key={task.id}>
                  <label className="mission-task-selection">
                    <input type="checkbox" aria-label={`选择任务 ${task.title}`} checked={selectedTaskIds.includes(task.id)} onChange={() => setSelectedTaskIds(current => toggleMissionTaskSelection(mission.proposed_tasks, current, task.id))} />
                    <span>纳入计划</span>
                  </label>
                  <div>
                    <strong>{task.title}</strong>
                    <small>{task.execution.role_id || '未指派专家'} · {task.execution.executor}{task.execution.model ? ` · ${task.execution.model}` : ''}</small>
                    <TaskExecutionEditor mission={mission} task={task} onSaved={next => { setMission(next); void poll(true); void onRefresh(); }} />
                    {task.depends_on.length ? <em>依赖：{task.depends_on.join('、')}</em> : null}
                    {task.write_paths.length ? <em>写入：{task.write_paths.join('、')}</em> : null}
                    {task.acceptance_criteria.length ? <em>验收：{task.acceptance_criteria.join('；')}</em> : null}
                    {task.required_checks.length ? <em>宿主检查：{task.required_checks.map(check => <code key={check}>{check}</code>)}</em> : null}
                  </div>
                </div>
              ))}
            </div>
          ) : mission.tasks.length ? (
            <div className="mission-task-list">
              {mission.tasks.map(task => {
                const taskVerificationLive = runningVerifications.some(current => current.spec.id === task.spec.id);
                const verificationStatus = task.status !== 'awaiting_verification' ? ''
                  : taskVerificationLive ? '独立核验进行中'
                  : resumeAction?.kind === 'verification' ? '待手动续跑核验'
                    : mission.status === 'waiting' ? '任务暂停，核验未继续'
                    : task.verification_run_id ? '核验任务已派发' : '待另一位专家核验';
                const statusHelp = task.status !== 'awaiting_verification' ? ''
                  : taskVerificationLive ? verificationExplanation
                  : resumeAction?.kind === 'verification' ? resumeAction.explanation
                    : mission.status === 'waiting' ? '后续派发已暂停；未确认关联核验仍在运行，不启动新核验。'
                    : task.verification_run_id ? '实现专家已提交结果；已派发检查与独立核验，尚需读取运行状态。'
                      : '实现专家已提交结果；系统将运行必需检查，并安排另一位专家核对。';
                const legacyOverride = legacyVerificationOverrideForTask(mission, events, task);
                const retryAvailable = canRetryWaitingVerification(mission, events, task);
                return (
                  <div className={`mission-task-row task-${task.status}`} key={task.spec.id} data-mission-task-id={task.spec.id}>
                    <span className="mission-task-status">{verificationStatus || MISSION_TASK_STATUS_LABELS[task.status]}</span>
                    <div>
                      <strong>{task.spec.title}</strong>
                      <small>{task.spec.execution.role_id || '未指派专家'} · {task.spec.execution.executor}{task.spec.execution.model ? ` · ${task.spec.execution.model}` : ''}</small>
                      {task.spec.depends_on.length ? <em>依赖：{task.spec.depends_on.map(id => taskTitle(mission, id)).join('、')}</em> : null}
                      {statusHelp ? <em>{statusHelp}</em> : null}
                      {task.status === 'failed' ? <em>{taskFailureReason(events, task.spec.id) || '执行或核验未通过，请查看活动记录。'}</em> : null}
                      <MissionTaskEvidence mission={mission} task={task} events={events}/>
                      {(canRevisit || retryAvailable) && task.verification_passed === false
                        && !mission.tasks.some(child => child.spec.repair_of === task.spec.id && child.status !== 'canceled')
                        && events.some(event => event.task_id === task.spec.id && ['task_baseline_captured', 'verification_retry_requested'].includes(event.event_type))
                        ? <button className="btn" disabled={Boolean(rechecking)} title={legacyOverride ? '本次明确覆盖只绑定已记录的暂停和运行失败事件；只重试同一实现的检查与独立核验' : '只重跑同一实现的必需检查与独立验收，不重做实现'} onClick={()=>void recheckTask(task.spec.id, task.verification_run_id, legacyOverride)}>{rechecking === task.spec.id ? '提交中' : legacyOverride ? '确认覆盖并重试核验' : mission.status === 'waiting' ? '重试核验并继续' : '重新核验同一实现'}</button> : null}
                    </div>
                  </div>
                );
              })}
            </div>
          ) : <p className="muted">确认计划后会在这里出现实际任务；空计划不会派发 Agent。</p>}
          {canRevisit && mission.budget && mission.tasks.filter(task => task.spec.repair_of).length < mission.budget.max_repair_attempts ? <button type="button" className="btn" title="生成失败交付的重做方案，保留原实现和核验历史" disabled={followingUp} onClick={() => void generateFollowUp()}>{followingUp ? <BusyIndicator size={14} /> : <Activity size={14} />}{followingUp ? '协调中' : '生成跟进任务'}</button> : null}
        </section>
      </div>
      {mission.status === 'active' && !verificationRunLive && mission.tasks.some(task => task.status === 'running' || task.status === 'awaiting_verification') ? (
        <div className="mission-recovery-control">
          <p>{mission.tasks.some(task => task.status === 'awaiting_verification')
            ? '宿主会自动派发独立验收。可立即检查已提交结果并运行必需检查，不重做实现。'
            : '继续原运行，避免重复执行。'}</p>
          <button type="button" className="btn" disabled={dispatching} onClick={() => void dispatchNext()}>
            {dispatching ? <BusyIndicator size={14} /> : <Activity size={14} />}
            {dispatching ? '检查中' : mission.tasks.some(task => task.status === 'awaiting_verification' && task.verification_run_id) ? '继续独立验收' : mission.tasks.some(task => task.status === 'awaiting_verification') ? '立即检查验收' : '恢复/检查运行'}
          </button>
        </div>
      ) : null}

        {mission.status === 'active' && !verificationRunLive && mission.tasks.some(task => task.status === 'ready') ? <button type="button" className="btn btn-primary" disabled={dispatching} onClick={() => void dispatchNext()}>{dispatching ? <BusyIndicator size={14} /> : <Plus size={14} />}{dispatching ? '执行中' : '继续执行下一项'}</button> : null}
        {relatedActivities.length ? <section className="mission-detail-panel"><h3>执行尝试与重试</h3><p className="muted">子任务及其验收执行归属于本任务，不另计顶层任务。</p>{relatedActivities.map(item=><div className="task-related-execution" key={item.id}><div><strong>{taskTitle(mission,item.mission_task_id||'') || item.title}</strong><small>{item.parent_kind==='retry'?'重试':item.parent_kind==='delegate'?'委派':'执行尝试'} · {activityStatusLabel(item)||taskStatusLabel(item.status)}</small><code>{item.workflow_run_id||item.id}</code></div>{onOpenWorkflowRun&&item.workflow_run_id?<button className="btn" onClick={()=>onOpenWorkflowRun(item.workflow_run_id!)}>查看执行</button>:null}</div>)}</section>:null}
      </div>:null}
      {tab==='experts'?<div role="tabpanel" aria-label="协作专家">
      {mission.workflow ? <section className="mission-detail-panel mission-assignment-summary">
        <div className="mission-detail-panel-head"><div><h3>协作专家</h3><p>{mission.assignment ? `已指派 ${mission.assignment.roles.length} 项职责` : '请选择专家及执行配置'}</p></div>
          {['awaiting_assignment', 'planned', 'awaiting_confirmation'].includes(mission.status) ? <button type="button" className="btn btn-primary" onClick={() => setAssignmentOpen(true)}>{mission.assignment ? '调整专家指派' : '选择协作专家'}</button> : null}
        </div>
        {mission.assignment ? <ul className="mission-assignment-summary-list">{mission.assignment.roles.map(role => <li key={role.role_id}>
          <strong>{role.label || role.role_id}</strong><span>{role.expert ? `${role.expert.expert_id} · ${role.expert.expert_version}` : '历史指派 · 未固定专家版本'}</span>
          <small>{role.executor}{role.service_id ? ` · ${role.service_id}` : ''}{role.model ? ` · ${role.model}` : ''}</small>
        </li>)}</ul> : null}
      </section> : null}
      {!mission.workflow?<p className="muted">此历史任务未绑定协作工作流。</p>:null}</div>:null}
      {tab==='activity'?<div role="tabpanel" aria-label="活动记录">
      <section className="mission-detail-panel mission-timeline-panel"><div className="mission-detail-panel-head"><div><h3>执行过程</h3><small>{progressDisclosure(mission, events)}</small></div><span className="mission-timeline-actions"><button type="button" className="btn" onClick={() => { setBusy(true); void poll(true).finally(() => setBusy(false)); }}>{busy ? <BusyIndicator size={14} /> : <Clock3 size={14} />}刷新</button></span></div>{events.length ? <ol className="mission-timeline">{events.map(event => <li key={event.event_id}><span className={`mission-timeline-dot type-${event.event_type}`} aria-hidden="true" /><div><strong>{eventLabel(event)}{event.task_id ? ` · ${taskTitle(mission, event.task_id)}` : ''}</strong><p>{event.summary}</p><time dateTime={event.occurred_at}>{new Date(event.occurred_at).toLocaleString()}</time></div></li>)}</ol> : <p className="muted">暂无细粒度执行事件，只显示任务状态。</p>}</section>
      </div>:null}
      {assignmentOpen ? <CollaborationAssignmentDialog mission={mission} onClose={() => setAssignmentOpen(false)} onAssigned={next => { setMission(next); onMissionChanged?.(next); setAssignmentOpen(false); setTab('experts'); void poll(true); void onRefresh(); }} /> : null}
      {runtimeWorkspaceOpen ? <RuntimeWorkspaceDialog mission={mission} onClose={() => setRuntimeWorkspaceOpen(false)} onResolved={next => { setMission(next); onMissionChanged?.(next); setRuntimeWorkspaceOpen(false); void poll(true); void onRefresh(); }} /> : null}
      {budgetExtensionOpen ? <MissionBudgetExtensionDialog mission={mission} events={events} historyComplete={historyComplete} now={now} reason={wait?.budget_reason || ''} onClose={() => setBudgetExtensionOpen(false)} onUpdated={next => { setMission(next); onMissionChanged?.(next); setBudgetExtensionOpen(false); setNotice(next.status === 'waiting' ? '预算已更新；任务仍在等待，请按当前原因处理。' : '预算已更新；继续尚未完成的任务，交付结果仍需检查与独立验收。'); void poll(true); void onRefresh(); }} /> : null}
      <div className="mission-detail-foot"><span>计划 {mission.plan_sha256 || '尚未生成'}</span><span>快照 {wait ? `${wait.contract_version} · ${wait.cursor}` : '关联未确认'}</span></div>
    </section>
  );
}

function MissionBudgetExtensionDialog({ mission, events, historyComplete, now, reason, onClose, onUpdated }: {
  mission: MissionView; events: MissionEvent[]; historyComplete: boolean; now: number; reason: string; onClose: () => void; onUpdated: (mission: MissionView) => void;
}) {
  const saved = pendingBudgetRequest(mission.mission_id, mission.plan_sha256);
  const [additionalRounds, setAdditionalRounds] = useState(String(saved?.additional_rounds || 0));
  const [additionalMinutes, setAdditionalMinutes] = useState(String(saved?.additional_minutes || 0));
  const [additionalRepairs, setAdditionalRepairs] = useState(String(saved?.additional_repair_attempts || 0));
  const [additionalTokens, setAdditionalTokens] = useState(saved?.additional_tokens == null ? '' : String(saved.additional_tokens));
  const [unlimitedTokens, setUnlimitedTokens] = useState(saved?.unlimited_tokens || false);
  const [additionalCost, setAdditionalCost] = useState(saved?.additional_cost == null ? '' : String(saved.additional_cost));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [authorizeLegacyRecovery, setAuthorizeLegacyRecovery] = useState(Boolean(saved?.legacy_verification_override));
  const tokenLimited = mission.budget?.max_total_tokens !== null && mission.budget?.max_total_tokens !== undefined;
  const costLimited = mission.budget?.max_cost !== null && mission.budget?.max_cost !== undefined;
  const legacyOverrideCandidate = historyComplete
    ? mission.tasks.map(task => legacyVerificationOverrideForTask(mission, events, task)).find(value => value !== null) || null
    : null;
  const budgetAction = legacyBudgetExtensionAction(Boolean(legacyOverrideCandidate), authorizeLegacyRecovery);

  async function submit() {
    if (busy || !mission.budget) return;
    setBusy(true);
    setError('');
    try {
      const parseInteger = (value: string, label: string, maximum: number) => {
        const parsed = Number(value || '0');
        if (!Number.isSafeInteger(parsed) || parsed < 0 || parsed > maximum) throw new Error(`${label}必须是有效的非负整数`);
        return parsed;
      };
      if (legacyOverrideCandidate && !authorizeLegacyRecovery) {
        throw new Error('请明确授权本次覆盖旧版暂停归属缺失');
      }
      const legacyTimeOnly = Boolean(legacyOverrideCandidate);
      const rounds = legacyTimeOnly ? 0 : parseInteger(additionalRounds, '追加轮次', 4_294_967_295);
      const minutes = parseInteger(additionalMinutes, '追加时长', 525_600);
      const repairs = legacyTimeOnly ? 0 : parseInteger(additionalRepairs, '追加返工次数', 4_294_967_295);
      const tokens = legacyTimeOnly ? null : !unlimitedTokens && additionalTokens.trim() ? parseInteger(additionalTokens, '追加 Token', 9_000_000_000_000_000) : null;
      const cost = legacyTimeOnly ? null : additionalCost.trim() ? Number(additionalCost) : null;
      if (cost !== null && (!Number.isFinite(cost) || cost <= 0)) throw new Error('追加成本额度必须是大于 0 的有效数字');
      if (unlimitedTokens && tokens !== null) throw new Error('请选择追加 Token 或不限 Token');
      if (!rounds && !minutes && !repairs && !tokens && !unlimitedTokens && !cost) throw new Error('请至少增加一项预算');
      const timeOnly = rounds === 0 && minutes > 0 && repairs === 0 && tokens === null && !unlimitedTokens && cost === null;
      if (authorizeLegacyRecovery && (!legacyOverrideCandidate || !timeOnly)) {
        throw new Error('旧记录人工覆盖只允许在完整事件记录下单独增加核验时限');
      }
      const input = retainBudgetRequest(mission.mission_id, {
        expected_plan_sha256: mission.plan_sha256,
        additional_rounds: rounds,
        additional_minutes: minutes,
        additional_repair_attempts: repairs,
        additional_tokens: tokens,
        unlimited_tokens: legacyTimeOnly ? false : unlimitedTokens,
        additional_cost: cost,
        ...(authorizeLegacyRecovery && legacyOverrideCandidate ? { legacy_verification_override: legacyOverrideCandidate } : {}),
      });
      const next = await agentApi.increaseMissionBudget(mission.mission_id, input);
      clearBudgetRequest(mission.mission_id, mission.plan_sha256);
      onUpdated(next);
    } catch (cause) {
      setError(formatError(cause, '预算更新失败'));
    } finally {
      setBusy(false);
    }
  }

  return <CollaborationDialog titleId="mission-budget-title" onClose={onClose}>
    <section className="collaboration-create-dialog collaboration-budget-dialog">
      <header><div><span>{missionWait(mission)?.reason === 'budget_exhausted' ? '预算已用尽' : '主动调整本任务预算'}</span><h2 id="mission-budget-title">{legacyOverrideCandidate ? '补充核验时限' : '增加预算并继续'}</h2></div><button type="button" className="btn" onClick={onClose}>关闭</button></header>
      <p className="mission-budget-reason">{reason || '仅追加你明确填写的额度；系统将复核所有限制及环境，成功后仍可能等待处理。'}</p>
      <MissionBudgetSummary mission={mission} events={events} historyComplete={historyComplete} now={now}/>
      {saved ? <p role="status">有待确认的增额申请。原参数可重试；改参数会新增额度，请先刷新核对上次结果。</p> : null}
      <div className="collaboration-create-fields mission-budget-fields">
        {legacyOverrideCandidate ? <label><span>追加时长（分钟）</span><input type="number" min="1" max="525600" step="1" value={additionalMinutes} onChange={event => setAdditionalMinutes(event.target.value)} /></label> : <>
        <label><span>追加派发轮次</span><input type="number" min="0" step="1" value={additionalRounds} onChange={event => setAdditionalRounds(event.target.value)} /></label>
        <label><span>追加返工次数</span><input type="number" min="0" step="1" value={additionalRepairs} onChange={event => setAdditionalRepairs(event.target.value)} /></label>
        <label><span>追加时长（分钟）</span><input type="number" min="0" max="525600" step="1" value={additionalMinutes} onChange={event => setAdditionalMinutes(event.target.value)} /></label>
        {tokenLimited ? <>
          <label><span>追加 Token</span><input type="number" min="1" step="1" value={additionalTokens} disabled={unlimitedTokens} onChange={event => setAdditionalTokens(event.target.value)} placeholder="留空表示不追加" /></label>
          <label className="mission-budget-toggle"><input type="checkbox" checked={unlimitedTokens} onChange={event => setUnlimitedTokens(event.target.checked)} /><span>移除 Token 上限</span></label>
        </> : null}
        {costLimited ? <label><span>追加成本额度</span><input type="number" min="0.000001" step="any" value={additionalCost} onChange={event => setAdditionalCost(event.target.value)} placeholder="留空表示不追加" /></label> : null}
        </>}
      </div>
      {legacyOverrideCandidate ? <>
        <label className="mission-budget-toggle"><input type="checkbox" checked={authorizeLegacyRecovery} onChange={event => setAuthorizeLegacyRecovery(event.target.checked)} /><span>我授权覆盖旧记录的暂停归属缺失，以重试同一交付核验</span></label>
        <p className="mission-budget-reason" role="status">{budgetAction.description}</p>
      </> : null}
      {error ? <p className="error" role="alert">{error}</p> : null}
      <footer><small>{legacyOverrideCandidate ? '不会重做实现或增加返工次数。' : '续跑尚未完成的任务；系统复核全部限额。响应丢失时请按原参数重试。'}</small><button type="button" className="btn btn-primary" disabled={busy || budgetAction.disabled} onClick={() => void submit()}>{busy ? <BusyIndicator size={14} /> : <Plus size={14} />}{busy ? '更新中' : budgetAction.label}</button></footer>
    </section>
  </CollaborationDialog>;
}

function mergeEvents(current: MissionEvent[], next: MissionEvent[]) {
  const byId = new Map(current.map(event => [event.event_id, event]));
  for (const event of next) byId.set(event.event_id, event);
  return [...byId.values()].sort((left, right) => left.sequence - right.sequence);
}

function CollaborationAssignmentDialog({ mission, onClose, onAssigned }: { mission: MissionView; onClose: () => void; onAssigned: (mission: MissionView) => void }) {
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
      setRoles(next.roles.map(role => {
        const existing = mission.assignment?.roles.find(value => value.role_id === role.role_id);
        if (existing) return { ...existing, label: role.label };
        const executor = next.executors.find(value => value.executor === role.recommended_executor);
        const recommendedModel = executor?.models.find(value => value.alias === role.recommended_model && value.service_id === role.recommended_service_id);
        return {
          role_id: role.role_id,
          label: role.label,
          expert: null,
          executor: role.recommended_executor,
          service_id: recommendedModel?.service_id || '',
          model: recommendedModel?.alias || '',
        };
      }));
      const fallback = next.roles.find(role => role.availability === 'available')?.role_id || next.roles[0]?.role_id || '';
      setTaskRoles(Object.fromEntries(mission.proposed_tasks.map(task => [task.id, task.execution.role_id || fallback])));
    }).catch(cause => setError(formatError(cause, '指派选项读取失败')));
    return () => { active = false; };
  }, [mission.mission_id]);

  function useRecommended() {
    if (!options) return;
    setRecommended(true);
    setRoles(current => options.roles.map(option => {
      const previous = current.find(role => role.role_id === option.role_id);
      const executor = options.executors.find(value => value.executor === option.recommended_executor);
      const recommendedModel = executor?.models.find(value => value.alias === option.recommended_model && value.service_id === option.recommended_service_id);
      return {
        ...(previous || { role_id: option.role_id, label: option.label, expert: null }),
        label: option.label,
        executor: option.recommended_executor,
        service_id: recommendedModel?.service_id || '',
        model: recommendedModel?.alias || '',
      };
    }));
  }

  async function submit() {
    if (!options || busy) return;
    const problems = roles.map(role => assignmentProblem(role, options)).filter(Boolean);
    const missingTaskRole = mission.proposed_tasks.some(task => !taskRoles[task.id]);
    if (problems.length) {
      setError(problems[0]);
      return;
    }
    if (missingTaskRole) {
      setError('请为每项计划任务选择承担专家');
      return;
    }
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

  const problems = options ? roles.map(role => ({ roleId: role.role_id, message: assignmentProblem(role, options) })).filter(item => item.message) : [];
  const missingTaskRole = mission.proposed_tasks.some(task => !taskRoles[task.id]);

  return (
    <CollaborationDialog titleId="mission-assignment-title" onClose={onClose}>
      <section className="collaboration-create-dialog collaboration-assignment-dialog" aria-labelledby="mission-assignment-title">
        <header>
          <div><span>固定工作流中的职责槽位</span><h2 id="mission-assignment-title">选择协作专家</h2><small>{options ? `${options.workflow_id}@${options.workflow_version}` : '正在读取固定工作流'}</small></div>
          <button type="button" className="btn" onClick={onClose}>关闭</button>
        </header>
        <p>选择各职责的专家和执行配置；随后单独确认计划与路径授权。</p>
        {error ? <p className="error" role="alert">{error}</p> : null}
        {options ? <div className="collaboration-assignment-list">{options.roles.map(option => {
          const value = roles.find(role => role.role_id === option.role_id);
          if (!value) return null;
          const selectedExpert = value.expert ? options.experts.find(expert => expert.id === value.expert?.expert_id && expert.version === value.expert?.expert_version && expert.digest === value.expert?.expert_digest) : undefined;
          const selectedExpertKey = value.expert ? `${value.expert.expert_id}@${value.expert.expert_version}:${value.expert.expert_digest}` : '';
          const selectedExecutor = options.executors.find(executor => executor.executor === value.executor);
          const services = serviceOptions(selectedExecutor);
          const models = (selectedExecutor?.models || []).filter(model => model.service_id === value.service_id);
          const problem = assignmentProblem(value, options);
          return <article className="collaboration-assignment-role" key={option.role_id}>
            <div className="collaboration-assignment-role-heading"><strong>{option.label || option.role_id}</strong><small>{option.brief || option.role_id}</small><em className={`collaboration-assignment-availability is-${option.availability}`}>{option.availability === 'available' ? '推荐配置可用' : option.unavailable_reason || '推荐配置不可用，可更换执行器'}</em>{problem ? <em className="collaboration-assignment-problem" role="status">{problem}</em> : null}</div>
            <label><span>专家 *</span><select aria-label={`${option.label || option.role_id}专家`} value={selectedExpertKey} onChange={event => { setRecommended(false); const expert = options.experts.find(item => expertKey(item) === event.target.value); setRoles(current => current.map(role => role.role_id === option.role_id ? bindExpert(role, expert) : role)); }}><option value="">请选择已安装专家</option>{options.experts.map(expert => <option key={expertKey(expert)} value={expertKey(expert)}>{expert.name} · {expert.version}</option>)}</select><small>{selectedExpert ? `摘要 ${selectedExpert.digest.slice(0, 12)}…` : '提交前必须选择专家版本'}</small></label>
            <label><span>执行器 *</span><select aria-label={`${option.label || option.role_id}执行器`} value={value.executor} onChange={event => { const executor = options.executors.find(item => item.executor === event.target.value); if (!executor) return; setRecommended(false); setRoles(current => current.map(role => role.role_id === option.role_id ? changeExecutor(role, executor) : role)); }}><option value="">请选择宿主执行器</option>{options.executors.map(executor => <option key={executor.executor} value={executor.executor} disabled={!executor.available}>{executor.label}{executor.available ? '' : ` · ${executor.reason || '不可用'}`}</option>)}</select></label>
            <label><span>服务 *</span><select aria-label={`${option.label || option.role_id}服务`} value={value.service_id} disabled={!selectedExecutor || selectedExecutor.models.length === 0} onChange={event => { setRecommended(false); setRoles(current => current.map(role => role.role_id === option.role_id ? changeService(role, selectedExecutor, event.target.value) : role)); }}><option value="">{services.length ? '请选择服务' : '由执行器自身配置'}</option>{services.map(service => <option key={service.id} value={service.id}>{service.label || service.id}</option>)}</select></label>
            <label><span>模型 *</span><select aria-label={`${option.label || option.role_id}模型`} value={value.model} disabled={!selectedExecutor || selectedExecutor.models.length === 0 || !value.service_id} onChange={event => { setRecommended(false); setRoles(current => current.map(role => role.role_id === option.role_id ? { ...role, model: event.target.value } : role)); }}><option value="">{models.length ? '请选择模型' : '由执行器自身配置'}</option>{models.map(model => <option key={model.alias} value={model.alias}>{model.label || model.alias}</option>)}</select></label>
          </article>;
        })}</div> : <div className="collaboration-task-loading"><BusyIndicator size={16} />正在读取职责目录</div>}
        {mission.proposed_tasks.length ? <div className="collaboration-task-role-map"><strong>任务与职责映射 *</strong>{mission.proposed_tasks.map(task => <label key={task.id}><span>{task.title}</span><select aria-label={`${task.title}承担专家`} value={taskRoles[task.id] || ''} onChange={event => { setRecommended(false); setTaskRoles(current => ({ ...current, [task.id]: event.target.value })); }}><option value="">请选择承担专家</option>{options?.roles.map(role => <option key={role.role_id} value={role.role_id}>{role.label || role.role_id}</option>)}</select></label>)}</div> : null}
        {missingTaskRole ? <p className="error" role="status">请为每项计划任务选择承担专家。</p> : null}
        <footer className="collaboration-assignment-actions"><small>专家版本与摘要随指派固定，内容变更需重新指派。</small><button type="button" className="btn" disabled={!options || busy} onClick={useRecommended}>使用推荐服务与模型</button><button type="button" className="btn btn-primary" disabled={!options || busy || roles.length === 0 || problems.length > 0 || missingTaskRole} onClick={() => void submit()}>{busy ? <BusyIndicator size={14} /> : <ShieldCheck size={14} />}{busy ? '应用中' : '应用角色指派'}</button></footer>
      </section>
    </CollaborationDialog>
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
