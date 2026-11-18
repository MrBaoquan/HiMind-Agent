import { useCallback, useEffect, useRef, useState, useLayoutEffect } from 'react';
import { Activity, CheckCircle2, ClipboardList, Clock3, Download, MessageCircle, PackageCheck, Plus, ShieldCheck, Users, X } from 'lucide-react';
import { BusyIndicator } from '../components/BusyIndicator';
import { EmptyState } from '../components/Common';
import { agentApi, type AgentActivityItem, type MissionAssignmentInput, type MissionAssignmentOptions, type WorkflowCenterItem, type MissionBudgetIncrease, type MissionEvent, type MissionExecutionOption, type MissionMessageInput, type MissionPlanChangeProposal, type MissionRoleAssignment, type MissionSnapshot, type MissionTaskSpec, type MissionView } from '../services/agentApi';
import { formatError } from '../types';
import { formatRelativeStamp, useNowTick } from '../timeFormat';
import { ENDED_STATUSES, executionTodoId, missionTodo, clientLabel, activityStatusLabel } from './taskCenterModel';
import { taskStatusLabel } from './taskView';
import { MissionEvidence, MissionTaskEvidence } from './MissionEvidence';
import { MissionBudgetSummary } from './MissionBudgetSummary';
import { MissionExecutionOverview, executionEventLabel as eventLabel } from './MissionExecutionOverview';
import { readMissionSnapshot } from './missionSnapshot';
import { pendingBudgetRequest, retainBudgetRequest, clearBudgetRequest } from './missionBudgetRequest';
import { RuntimeWorkspaceDialog } from './RuntimeWorkspaceDialog';
import { CollaborationDialog } from './CollaborationDialog';
import { assignmentProblem, bindExpert, changeExecutor, changeService, expertKey, hasMissingTaskAssignment, initialAssignmentRoles, initialTaskRoles, recommendedRole, serviceOptions } from './missionAssignment';
import {
  canRetryWaitingVerification,
  legacyBudgetExtensionAction,
  legacyVerificationOverrideForTask,
  missionNextAction,
  missionRunningTasks,
  missionTaskActivity,
  missionRunningVerifications,
  missionTaskExecutionLabel,
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
  progressDisclosure,
  taskTitle,
} from './missionView';

const COLLAB_ACTIVE_STATUSES = new Set(['draft', 'awaiting_assignment', 'planned', 'awaiting_confirmation', 'active', 'waiting']);

function MissionWorkCard({ mission, task, activities, fresh, event, onSelect }: { mission: MissionView; task: MissionView['tasks'][number]; activities: AgentActivityItem[]; fresh: boolean; event?: MissionEvent; onSelect: () => void }) {
  const role = mission.assignment?.roles.find(value => value.role_id === task.spec.execution.role_id);
  const label = role?.label || task.spec.execution.role_id || '协作专家';
  const initials = Array.from(label.replace(/专家|工程师|设计师/g, '').trim()).slice(0, 2).join('') || 'AI';
  const activity = fresh ? missionTaskActivity(mission, task, activities) : undefined;
  const live = activity?.status === 'running';
  const status = !fresh && ['running','awaiting_verification'].includes(task.status) ? '运行状态待核实' : missionTaskExecutionLabel(mission, task, fresh ? activities : []);
  const dependencies = task.spec.depends_on.map(id => mission.tasks.find(item => item.spec.id === id));
  const waiting = dependencies.filter(item => item?.status !== 'completed').map(item => item?.spec.title || '前置任务');
  const [motion, setMotion] = useState(false);
  useEffect(() => {
    if (!fresh || !event || Date.now()-Date.parse(event.occurred_at)>10000) return;
    setMotion(true); const timer = window.setTimeout(() => setMotion(false), 650);
    return () => window.clearTimeout(timer);
  }, [event?.event_id]);
  return <button type="button" className={`mission-work-card${live ? ' is-running' : ''}${motion ? ' has-new-event' : ''}`} data-task-id={task.spec.id} data-runtime-live={live} onClick={onSelect}>
    <div className="mission-work-card-head"><span className="mission-expert-avatar" aria-hidden="true">{initials}</span><span className="mission-work-expert">{label}<small>{task.spec.execution.executor === 'personal.codex' ? 'Codex' : task.spec.execution.executor === 'himind.builtin' ? 'HiMind AI' : task.spec.execution.executor}</small></span><span className={`mission-work-status is-${task.status}`}>{status}</span></div>
    <strong>{task.spec.title}</strong><p>{task.spec.objective || task.spec.acceptance_criteria[0] || '执行已确认的任务要求'}</p>
    {dependencies.length ? <span className="mission-dependency-chips">{dependencies.map((dependency,index) => <small key={task.spec.depends_on[index]}>{dependency?.status === 'completed' ? '✓ 已交接' : '← 等待'} · {dependency?.spec.title || task.spec.depends_on[index]}</small>)}</span> : null}
    <small className="mission-work-activity">{!fresh ? '连接状态待核实，显示上次快照' : live ? <><span className="mission-live-dot"/>执行器已确认正在处理</> : task.status === 'running' ? status : task.status === 'blocked' ? `等待 ${waiting.join('、') || '资源、计划变更或调度容量'}` : task.status === 'awaiting_verification' ? '实现已交付 · 由另一执行实例检查实际成果' : task.status === 'completed' ? '宿主检查及独立验收已通过' : task.status === 'failed' ? '执行失败 · 查看证据与下一步' : task.status === 'ready' ? '可派发 · 等待调度' : '按确认后的任务计划执行'}</small>
  </button>;
}

function MissionCoordination({ mission, events, busy, message, onMessageChange, scrollPosition, focusChange, onSend, onApply, onWithdraw }: {
  mission: MissionView; events: MissionEvent[]; busy: boolean;
  message: string; onMessageChange: (value: string) => void; scrollPosition: { current: number };
  focusChange: { id: string; revision: number } | null;
  onSend: (input: MissionMessageInput) => Promise<void>;
  onWithdraw: (requestId: string) => Promise<void>;
  onApply: (proposal: MissionPlanChangeProposal, readPaths: string[], writePaths: string[]) => Promise<void>;
}) {
  const conversationRef = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => { if (conversationRef.current) conversationRef.current.scrollTop = scrollPosition.current; }, [scrollPosition]);
  useLayoutEffect(() => {
    if (!focusChange) return;
    const card = Array.from(conversationRef.current?.querySelectorAll<HTMLElement>('[data-change-id]') || [])
      .find(item => item.dataset.changeId === focusChange.id);
    card?.scrollIntoView({block:'nearest'});
  }, [focusChange]);
  const messages = events.filter(event => event.event_type === 'mission_message_submitted');
  const progressByRequest = new Map<string, { status?: string; answer?: string }>();
  for (const event of events.filter(item => item.event_type === 'mission_instruction_status_changed')) {
    const payload = event.payload as { request_id?: string; status?: string; answer?: string };
    if (payload.request_id) progressByRequest.set(payload.request_id, payload);
  }
  const proposals = events.filter(event => event.event_type === 'mission_plan_change_proposed').map(event => {
    const payload = event.payload as { proposal?: MissionPlanChangeProposal; status?: string; read_scope_delta?: string[]; write_scope_delta?: string[] };
    return { proposal: payload.proposal, status: payload.status || 'proposed', reads: payload.read_scope_delta || [], writes: payload.write_scope_delta || [] };
  }).filter((item): item is { proposal: MissionPlanChangeProposal; status: string; reads: string[]; writes: string[] } => Boolean(item.proposal));
  const statusLabel = (status?: string) => ({
    needs_clarification: '请澄清含义', awaiting_checkpoint: '受影响工作收尾后生效', received: '已接收', analyzing: '正在分析', awaiting_authorization: '待确认新增访问范围',
    awaiting_confirmation: '待确认变更', ready_to_apply: '正在提交', applied: '已应用',
    partially_applied: '部分生效', not_applied: '未应用', failed: '失败', answered: '已回答', recorded: '已记录',
  } as Record<string, string>)[status || ''] || status || '已记录';
  return <section className="mission-coordination" aria-label="与协调助手沟通">
    <header><div><span className="mission-coordination-icon"><MessageCircle size={17}/></span><div><h3>与协调助手沟通</h3><p>随时询问进展或补充要求，执行与沟通分别跟踪。</p></div></div></header>
    <div className="mission-conversation" ref={conversationRef} onScroll={event => { scrollPosition.current = event.currentTarget.scrollTop; }} aria-live="polite">{messages.length ? messages.map(event => { const payload = event.payload as { request_id?: string; input?: { kind?: string; message?: string }; status?: string; answer?: string }; const progress = progressByRequest.get(payload.request_id || ''); return <article key={event.event_id} className="mission-message-entry">
      <small>{payload.input?.kind === 'question' ? '你 · 询问' : payload.input?.kind === 'instruction' ? '你 · 要求' : '你'} · {statusLabel(progress?.status || payload.status)}</small>
      <p>{payload.input?.message || event.summary}</p><div>{progress?.answer || payload.answer || '等待协调助手处理'}</div>{(progress?.status || payload.status)==='needs_clarification' && payload.input?.message ? <div className="mission-clarification-actions">{(['question','instruction'] as const).map(kind=><button className="btn" disabled={busy} key={kind} onClick={()=>void onSend({request_id:`agent-ui:clarification:${crypto.randomUUID()}`,expected_plan_revision:mission.plan_revision,expected_plan_sha256:mission.plan_sha256,kind,message:payload.input!.message!}).catch(()=>{})}>{kind==='question'?'只是询问':'这是明确执行要求'}</button>)}</div> : null}
      {payload.request_id && ['received','analyzing','awaiting_checkpoint','awaiting_authorization','awaiting_confirmation','ready_to_apply'].includes(progress?.status || payload.status || '') ? <button className="btn" disabled={busy} onClick={()=>void onWithdraw(payload.request_id!)}>撤回未生效要求</button> : null}
    </article>;}) : <p className="mission-conversation-empty">暂无沟通记录</p>}
    {proposals.map(({ proposal, status: storedStatus, reads, writes }) => {
      const status = progressByRequest.get(proposal.instruction_request_id)?.status || storedStatus;
      const canApply = ['awaiting_authorization', 'awaiting_confirmation'].includes(status)
        && proposal.expected_plan_revision === mission.plan_revision && proposal.expected_plan_sha256 === mission.plan_sha256;
      return <article className="mission-change-review" key={proposal.request_id} data-change-id={proposal.request_id}>
        <header><div><strong>{proposal.summary}</strong><small>计划 r{proposal.expected_plan_revision} · {statusLabel(status)}</small></div></header>
        {proposal.rationale ? <p>{proposal.rationale}</p> : null}
        {proposal.added_roles.length ? <p>新增专家：{proposal.added_roles.map(role => role.label).join('、')}</p> : null}
        {proposal.added_tasks.length ? <ul>{proposal.added_tasks.map(task => <li key={task.id}>{task.title}{task.depends_on.length ? ` · 等待 ${task.depends_on.join('、')}` : ' · 可并行'}</li>)}</ul> : null}
        {proposal.updates?.length ? <div className="mission-change-updates"><strong>调整已有工作</strong>{proposal.updates.map(update => {
          const task = mission.tasks.find(item => item.spec.id === update.task_id);
          const role = [...(mission.assignment?.roles || []), ...proposal.added_roles].find(item => item.role_id === update.role_id);
          const dependencyTitle = (id: string) => [...mission.tasks.map(item => item.spec), ...proposal.added_tasks].find(item => item.id === id)?.title || id;
          return <article key={update.task_id}>
            <strong>{task?.spec.title || update.task_id}</strong>
            {update.title != null ? <p>标题改为：{update.title}</p> : null}
            {update.objective != null ? <p>目标：{update.objective}</p> : null}
            {update.depends_on != null ? <p>依赖改为：{update.depends_on.length ? update.depends_on.map(dependencyTitle).join('、') : '无前置依赖'}</p> : null}
            {update.role_id != null ? <p>改派：{role?.label || update.role_id}</p> : null}
            {update.priority != null ? <p>优先级：{update.priority}</p> : null}
            {update.acceptance_criteria != null ? <p>验收改为：{update.acceptance_criteria.join('；') || '无单项验收声明'}</p> : null}
            {update.required_checks != null ? <p>检查改为：{update.required_checks.join('；') || '无指定命令'}</p> : null}
            {task && ['running', 'awaiting_verification'].includes(task.status) ? <small>当前工作收尾后重新执行</small> : null}
          </article>;
        })}</div> : null}
        {proposal.cancel_task_ids?.length ? <p>撤销未启动工作：{proposal.cancel_task_ids.map(id => mission.tasks.find(task => task.spec.id === id)?.spec.title || id).join('、')}</p> : null}
        {proposal.required_acceptance?.length ? <p>新增整体验收：{proposal.required_acceptance.join('；')}</p> : null}
        {reads.length || writes.length ? <div className="mission-change-scope"><strong>新增访问授权</strong>{reads.map(path => <code key={`r:${path}`}>读取 · {path}</code>)}{writes.map(path => <code key={`w:${path}`}>写入 · {path}</code>)}</div> : null}
        {canApply ? <button type="button" className="btn btn-primary" disabled={busy} onClick={() => void onApply(proposal, reads, writes)}>{busy ? <BusyIndicator size={14}/> : <CheckCircle2 size={14}/>}确认并应用这项变更</button>
          : !['applied', 'partially_applied', 'not_applied', 'failed', 'ready_to_apply', 'awaiting_checkpoint'].includes(status) ? <small>计划版本已变化；此提案不能应用，需要按最新计划重新分析。</small> : null}
      </article>;
    })}
    </div>
    <form onSubmit={event => { event.preventDefault(); if (!message.trim() || busy) return; void onSend({request_id:`agent-ui:communication:${crypto.randomUUID()}`, expected_plan_revision:mission.plan_revision, expected_plan_sha256:mission.plan_sha256, kind:'message', message:message.trim()}).then(()=>onMessageChange('')).catch(()=>{}); }}>
      <textarea aria-label="发给协调助手的消息" value={message} onChange={event=>onMessageChange(event.target.value)} placeholder="询问进展，或提出要求，例如：新增离线运行支持，界面工作继续" maxLength={8000}/>
      <footer><small>询问不会改动计划；要求经宿主校验并提交后才生效。</small><button className="btn btn-primary" disabled={busy || !message.trim()}>{busy?'提交中':'发送'}</button></footer>
    </form>
  </section>;
}

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
    <p>运行中计划调整：{mission.workspace.adjustment_mode === 'confirm_changes' ? '每次变更前确认' : '已授权范围内自动调整'}</p>
    <p>{full ? <>按当前用户权限访问工作区外文件；ACP 请求自动批准。<br />目标与验收标准仍有效。</> : 'HiMind AI 隔离工作区；外部执行器自行控制访问。受阻时可调整权限。'}</p>
    {editable ? <small>确认计划后生效，无需逐次确认；不会自动提权。</small> : null}
    {error ? <p role="alert" className="error">{error}</p> : null}
  </section>;
}

export function CreateMissionDialog({ onClose, onCreated }: { onClose: () => void; onCreated: (mission: MissionView) => void }) {
  const [title, setTitle] = useState('');
  const [objective, setObjective] = useState('');
  const [root, setRoot] = useState('');
  const [writeScope, setWriteScope] = useState('.');
  const [permissionMode, setPermissionMode] = useState<'workspace_write' | 'full_access'>('workspace_write');
  const [adjustmentMode, setAdjustmentMode] = useState<'automatic' | 'confirm_changes'>('automatic');
  const [acceptance, setAcceptance] = useState('');
  const [workflows, setWorkflows] = useState<WorkflowCenterItem[]>([]);
  const [workflowId, setWorkflowId] = useState('');
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [workflowError, setWorkflowError] = useState('');
  const [workflowLoading, setWorkflowLoading] = useState(false);
  const requestId = useRef(`agent-ui:${crypto.randomUUID()}`);
  useEffect(() => {
    if (!advancedOpen) return;
    let active = true;
    setWorkflowLoading(true); setWorkflowError('');
    void agentApi.workflowCenter().then(snapshot => {
      if (active) setWorkflows(snapshot.workflows.filter(item => item.enabled && !item.development && (Object.keys(item.package.roles || {}).length > 0 || item.package.steps.some(step => step.role_id?.trim()))));
    }).catch(cause => { if (active) setWorkflowError(formatError(cause, '固定工作流读取失败，可继续自动组队')); })
      .finally(() => { if (active) setWorkflowLoading(false); });
    return () => { active = false; };
  }, [advancedOpen]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  async function submit() {
    if (busy || !objective.trim() || !root.trim()) return;
    setBusy(true);
    setError('');
    try {
      const selected = workflows.find(item => item.package.id === workflowId);
      if (workflowId && !selected) throw new Error('所选固定工作流不可用，请重新选择或使用自动组队');
      const input = {
        idempotency_key: requestId.current, source_client: 'agent-ui', source_reference: requestId.current,
        title: title.trim() || Array.from(objective.trim().split('\n')[0]).slice(0, 120).join(''), objective: objective.trim(),
        workspace: { root: root.trim(), permission_mode: permissionMode, adjustment_mode: adjustmentMode, read_scope: ['.'], write_scope: writeScope.split(/[,\n]/).map(value => value.trim()).filter(Boolean) },
        acceptance_criteria: acceptance.split('\n').map(value => value.trim()).filter(Boolean),
      };
      onCreated(await (selected ? agentApi.createMissionHandoff({
        ...input,
        workflow: { workflow_id: selected.package.id, workflow_version: selected.package.version, workflow_sha256: selected.package_digest, entrypoint: selected.package.default_entrypoint || '' },
        plan_mode: 'generated',
      }) : agentApi.createMissionIntent(input)));
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
          <label><span>需求描述 *</span><textarea required value={objective} onChange={event => setObjective(event.target.value)} placeholder="描述要完成的工作、预期结果和约束" /></label>
          <p className="muted">默认根据需求自动组队，创建后可调整专家、执行器、服务与模型。</p>
          <label><span>任务标题（可选）</span><input value={title} onChange={event => setTitle(event.target.value)} placeholder="留空时使用需求首行" /></label>
          <label><span>工作区根目录 *</span><input required value={root} onChange={event => setRoot(event.target.value)} placeholder="项目的本地绝对路径" /></label>
          <label><span>执行权限</span><select value={permissionMode} onChange={event => setPermissionMode(event.target.value as 'workspace_write' | 'full_access')}><option value="workspace_write">工作区访问</option><option value="full_access">完全访问</option></select></label>
          <small>默认允许修改工作区；确认计划后执行，可在进阶选项缩小范围。</small>
          <small>{permissionMode === 'full_access' ? <>确认计划后，HiMind AI 或 ACP 按当前用户权限运行。<br />ACP 请求自动批准；不会自动提权为管理员。</> : 'HiMind AI 使用工作区沙箱；外部执行器遵循自身访问控制。'}</small>
          <label><span>完成标准</span><textarea value={acceptance} onChange={event => setAcceptance(event.target.value)} placeholder="每行一条可验证标准" /></label>
          <details onToggle={event => setAdvancedOpen(event.currentTarget.open)}>
            <summary>进阶选项：固定工作流与修改范围</summary>
            <label><span>固定工作流（可选）</span><select value={workflowId} onChange={event => setWorkflowId(event.target.value)}><option value="">自动组队（推荐）</option>{workflows.map(item => <option key={item.package.id} value={item.package.id}>{item.package.name} · {item.package.version}</option>)}</select></label>
            {workflowLoading ? <small>正在读取固定工作流</small> : workflowError ? <p className="muted" role="status">{workflowError}</p> : !workflows.length ? <small>暂无已安装的协作工作流，可继续自动组队。</small> : null}
            <label><span>运行中计划调整</span><select value={adjustmentMode} onChange={event => setAdjustmentMode(event.target.value as 'automatic' | 'confirm_changes')}><option value="automatic">授权范围内自动调整</option><option value="confirm_changes">每次变更前确认</option></select></label>
            <small>自动调整遵循现有授权；新增访问范围需另行确认。</small>
            <label><span>允许修改的范围（可选）</span><textarea value={writeScope} onChange={event => setWriteScope(event.target.value)} placeholder="例如：frontend/src，行或逗号分隔；后续确认计划时核对" /></label>
          </details>
        </div>
        {error ? <p className="error" role="alert">{error}</p> : null}
        <footer><small>创建草案不会启动执行；随后调整指派、确认计划与权限。</small><button type="button" className="btn btn-primary" disabled={busy || !objective.trim() || !root.trim()} onClick={() => void submit()}>{busy ? <BusyIndicator size={14} /> : <Plus size={14} />}{busy ? '创建中' : '创建草案'}</button></footer>
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
  const [tab, setDetailTab] = useState<'overview' | 'plan' | 'experts' | 'activity'>('overview');
  const [viewTab, setViewTab] = useState<'collaboration' | 'delivery'>('collaboration');
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
  const [contextPanel, setContextPanel] = useState<'none' | 'coordination' | 'detail'>('none');
  const coordinationOpen = contextPanel === 'coordination';
  const detailsOpen = contextPanel === 'detail';
  const assistantOpenedForMission = useRef('');
  const [messageDraft, setMessageDraft] = useState('');
  const conversationScroll = useRef(0);
  const [selectedWorkId, setSelectedWorkId] = useState('');
  const [focusChange, setFocusChange] = useState<{ id: string; revision: number } | null>(null);
  const setTab = (next: typeof tab) => { setDetailTab(next); setContextPanel('detail'); };
  const openTask = (id: string) => { setSelectedWorkId(id); setTab('plan'); };
  const [lastReceivedAt, setLastReceivedAt] = useState(0);
  const [connectionFailed, setConnectionFailed] = useState(false);
  const completedView = useRef('');
  useEffect(() => { if (mission?.status === 'completed' && completedView.current !== mission.mission_id) {
    completedView.current=mission.mission_id; setViewTab('delivery');
  } }, [mission?.mission_id, mission?.status]);
  useEffect(() => {
    if (!mission || !['active', 'waiting'].includes(mission.status)
        || assistantOpenedForMission.current === mission.mission_id) return;
    assistantOpenedForMission.current = mission.mission_id;
    setContextPanel('coordination');
  }, [mission?.mission_id, mission?.status]);

  const [messageBusy, setMessageBusy] = useState(false);
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
      setLastReceivedAt(Date.now()); setConnectionFailed(false);
      setMission(snapshot.mission);
      onMissionChanged?.(snapshot.mission);
      setEvents(current => reset ? snapshot.events : mergeEvents(current, snapshot.events));
      setHistoryComplete(current => (reset || current) && !snapshot.requires_snapshot && !mismatched && snapshot.mission.effective_wait?.cursor === snapshot.cursor);
      cursorRef.current = snapshot.cursor;
      setError(mismatched ? '状态快照关联不一致，增额与恢复资格暂未确认；请刷新。' : '');
    } catch (cause) {
      if (revision !== pollRevision.current) return;
      setMission(current => current ? {...current, effective_wait: null} : current);
      setHistoryComplete(false); setConnectionFailed(true);
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
  const now = useNowTick(true);
  if (!mission) return <section className="mission-detail"><button type="button" className="btn" onClick={onBack}>返回我的任务</button>{error ? <p className="error" role="alert">{error}</p> : <div className="collaboration-task-loading"><BusyIndicator size={18} />正在读取任务详情</div>}</section>;
  const stage = missionStage(mission);
  const progress = missionProgress(mission);
  const runningTasks = missionRunningTasks(mission, relatedActivities);
  const flow = missionFlow(mission);
  const verificationExplanation = missionVerificationExplanation(mission, events, relatedActivities);
  const currentFlowIndex = flow.findIndex(step => step.state === 'current');
  const currentStageIsVerification = currentFlowIndex === 3;
  const runningVerifications = missionRunningVerifications(mission, relatedActivities);
  const verificationRunLive = runningVerifications.length > 0;
  const verificationStarted = progress.awaitingVerification > 0 && progress.startedVerification > 0;
  const verificationQueued = mission.status === 'active' && progress.awaitingVerification > 0 && !verificationStarted;
  const liveWork = !connectionFailed && lastReceivedAt > 0 && now-lastReceivedAt < 20000 && (runningTasks.length > 0 || verificationRunLive);
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

  async function sendCoordinationMessage(input: MissionMessageInput) {
    setMessageBusy(true);
    try {
      await agentApi.submitMissionMessage(missionId, input);
      // The durable conversation carries the reply; do not repeat it above the workspace.
      await poll(true);
    } catch (cause) {
      setError(formatError(cause, '沟通消息提交失败'));
      throw cause;
    } finally { setMessageBusy(false); }
  }

  async function applyCoordinationChange(proposal: MissionPlanChangeProposal, readPaths: string[], writePaths: string[]) {
    setMessageBusy(true);
    setError('');
    try {
      const result = await agentApi.applyMissionPlanChange({
        mission_id: proposal.mission_id, request_id: proposal.request_id,
        expected_plan_revision: proposal.expected_plan_revision, expected_plan_sha256: proposal.expected_plan_sha256,
        read_paths: readPaths, write_paths: writePaths,
      });
      setNotice(result.status==='applied' ? `已应用：${proposal.summary}` : `已确认，受影响工作收尾后生效：${proposal.summary}`);
      await poll(true);
      await onRefresh();
    } catch (cause) { setError(formatError(cause, '计划变更未应用')); }
    finally { setMessageBusy(false); }
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
  const cardTasks = mission.status === 'awaiting_confirmation'
    ? mission.proposed_tasks.map(spec => ({ spec, status: 'proposed' as const, attempt_run_id: '', verification_run_id: '', verification_passed: null, evidence_refs: [] }))
    : mission.tasks;
  const completedCards = cardTasks.filter(task => task.status === 'completed');
  const currentCards = cardTasks.filter(task => task.status !== 'completed' && task.status !== 'canceled');
  const activeCards = currentCards.filter(task => ['running','awaiting_verification','failed'].includes(task.status));
  const upcomingCards = currentCards.filter(task => !['running','awaiting_verification','failed'].includes(task.status));
  const fresh = !connectionFailed && lastReceivedAt > 0 && now-lastReceivedAt < 20000;
  const actualConcurrency = fresh ? runningTasks.length + runningVerifications.length : null;
  const cardEvent = (id: string) => [...events].reverse().find(event => event.task_id===id && ['task_dispatched','task_completed','verification_started','verification_failed','task_candidate_integrated','mission_task_evidence_invalidated'].includes(event.event_type));
  const lastEvent = events.length ? events[events.length - 1] : null;
  const instructionStatuses = new Map<string, string>();
  for (const event of events.filter(item => item.event_type === 'mission_instruction_status_changed')) {
    const payload = event.payload as { request_id?: string; status?: string };
    if (payload.request_id && payload.status) instructionStatuses.set(payload.request_id, payload.status);
  }
  const pendingPlanChange = [...events].reverse().filter(event => event.event_type === 'mission_plan_change_proposed')
    .map(event => {
      const payload = event.payload as { proposal?: MissionPlanChangeProposal; status?: string };
      return payload.proposal ? { proposal: payload.proposal, status: instructionStatuses.get(payload.proposal.instruction_request_id) || payload.status || 'proposed' } : null;
    }).find(item => item && ['awaiting_authorization', 'awaiting_confirmation'].includes(item.status));
  const pendingChangeIsCurrent = Boolean(pendingPlanChange && pendingPlanChange.proposal.expected_plan_revision === mission.plan_revision
    && pendingPlanChange.proposal.expected_plan_sha256 === mission.plan_sha256);
  return (
    <section className="mission-detail mission-workbench" data-mission-id={mission.mission_id} data-todo-id={todo?.id} data-snapshot-cursor={wait?.cursor} data-wait-reason={wait?.reason} data-wait-actor={missionActionActor(mission, relatedActivities)}>
      <div className="mission-detail-head"><button type="button" className="btn" onClick={onBack}>返回我的任务</button><span className={'collaboration-task-status status-'+mission.status}>{fresh ? detailStatusLabel : '状态待核实'}</span>        <details className="mission-more-actions"><summary>任务操作</summary>
          {pendingBudget || wait?.can_increase_budget ? <button className="btn" onClick={()=>setBudgetExtensionOpen(true)}>{pendingBudget ? '重试原增额申请' : '主动增加预算并续跑'}</button> : null}
          <p>续跑继续未完成任务；重核只检查同一实现。<br />重做按方案修改交付，保留历史。</p>
          {mission.status==='active'?<button className="btn" disabled={controlling} onClick={()=>void controlMission('pause')}>暂停后续执行</button>:null}
          {COLLAB_ACTIVE_STATUSES.has(mission.status)?<button className="btn" disabled={controlling} onClick={()=>void controlMission('cancel')}>取消任务</button>:null}
          <button className="btn" onClick={()=>{setBusy(true);void poll(true).finally(()=>setBusy(false));}}>{busy?'刷新中':'刷新任务'}</button>
        </details></div>
      <header className="mission-detail-hero"><div className="mission-detail-hero-copy"><h2>{mission.title}</h2><p>{fresh ? missionResponsibility(mission, relatedActivities) : '状态连接中断或快照过期，正在核实当前执行；请稍候或刷新。'}</p></div>
        <div className="mission-detail-progress"><strong>{progress.total || cardTasks.length ? `${progress.done}/${progress.total || cardTasks.length}` : '待规划'}</strong><span>已完成 · {actualConcurrency === null ? '待核实' : `${actualConcurrency} 项实际执行`} · {lastEvent ? `最近：${eventLabel(lastEvent)} ${formatRelativeStamp(lastEvent.occurred_at, now)}` : '尚无活动记录'}</span></div>
      </header>
      {error ? <p className="error" role="alert">{error}</p> : null}
      {!fresh ? <p className="mission-connection-status" role="status">{connectionFailed ? '连接中断' : '快照已过期'} · 保留上次记录，当前运行情况待核实。</p> : null}
      {notice ? <p role="status">{notice}</p> : null}
      {(pendingPlanChange || todo || mission.status === 'waiting' || mission.status === 'failed' || missionStalled(mission) || verificationExplanation) ? <div className="mission-current-action" role="status" aria-live="polite" aria-atomic="true">
        <div><strong>{pendingPlanChange ? '计划变更待你确认' : <>{missionNextAction(mission, events, relatedActivities)}</>}</strong><p>{pendingPlanChange ? `${pendingPlanChange.proposal.summary}${pendingChangeIsCurrent ? '。请查看影响和新增授权范围。' : '。计划已变化，此提案需按最新状态重新分析。'}` : ''}</p>{verificationExplanation ? <p>{verificationExplanation}</p> : ['waiting','failed'].includes(mission.status) || missionStalled(mission) ? <p>{wait?.budget_reason || (runtimeBlocked ? '命令环境需要处理；授权范围由你选择。' : '以当前快照为准，活动记录仅供追溯。')}</p> : null}</div>
        {pendingPlanChange ? <button className="btn btn-primary" onClick={()=>{setFocusChange({id:pendingPlanChange.proposal.request_id,revision:Date.now()});setContextPanel('coordination');}}>{pendingChangeIsCurrent ? '查看并确认变更' : '查看变更冲突'}</button>
        : mission.status==='awaiting_assignment' ? <button className="btn btn-primary" onClick={()=>setAssignmentOpen(true)}>选择协作专家</button>
        : ['draft','planned'].includes(mission.status) ? <button className="btn btn-primary" disabled={planning} onClick={()=>void generatePlan()}>{planning?'生成中':'生成执行计划'}</button>
        : mission.status==='awaiting_confirmation' ? <button className="btn btn-primary" onClick={()=>setTab('plan')}>审查计划与访问范围</button>
        : (verificationRunLive || mission.status==='active') && progress.awaitingVerification > 0 ? <button className="btn btn-primary" onClick={()=>setTab('plan')}>查看验收详情</button>
        : mission.status==='active' && progress.running > 0 ? <button className="btn" onClick={()=>setTab('overview')}>查看执行现场</button>
        : missionStalled(mission) ? <button className="btn btn-primary" onClick={()=>setTab('plan')}>查看失败原因</button>
        : runtimeBlocked ? <button className="btn btn-primary" onClick={()=>setRuntimeWorkspaceOpen(true)}>处理命令环境</button>
        : legacyRetry ? <button className="btn btn-primary" disabled={Boolean(rechecking)} onClick={()=>void recheckTask(legacyRetry.task.spec.id, legacyRetry.task.verification_run_id, legacyRetry.override)}>{rechecking === legacyRetry.task.spec.id ? '提交中' : '确认覆盖并重试核验'}</button>
        : mission.status==='waiting' && budgetExhausted && wait?.can_increase_budget ? <button className="btn btn-primary" onClick={()=>setBudgetExtensionOpen(true)}><Plus size={14}/>{legacyCandidates.length ? '补充核验时限' : '增加预算并继续'}</button>
        : resumeAction ? <button className={resumeAction.kind === 'verification' ? 'btn btn-primary' : 'btn'} disabled={controlling} onClick={()=>void controlMission('resume')}>{resumeAction.label}</button>
        : mission.status==='completed' ? <button className="btn" onClick={()=>{setViewTab('delivery');}}>查看交付结果</button>
        : mission.status==='failed' ? <button className="btn" onClick={()=>setTab('activity')}>查看失败原因</button> : null}

      </div> : null}
      {userRuns.length ? <section className="mission-detail-panel"><h3>需要你处理的执行事项</h3>{userRuns.map(item=><div className="task-related-execution" key={item.id} data-todo-id={executionTodoId(item.workflow_run_id || item.id, item.current_step_id || '', item.pause_kind || '')}><span>{taskTitle(mission,item.mission_task_id||'') || item.title} · {activityStatusLabel(item)}</span>{onOpenWorkflowRun&&item.workflow_run_id?<button className="btn btn-primary" onClick={()=>onOpenWorkflowRun(item.workflow_run_id!)}>处理执行事项</button>:null}</div>)}</section>:null}
      <div className="mission-workbench-nav">
        <div className="mission-workbench-tabs" role="tablist" aria-label="协作任务内容">
          <button role="tab" aria-selected={viewTab==='collaboration'} className={viewTab==='collaboration'?'active':''} onClick={()=>{setViewTab('collaboration');}}><Users size={15}/>协作</button>
          <button role="tab" aria-selected={viewTab==='delivery'} className={viewTab==='delivery'?'active':''} onClick={()=>setViewTab('delivery')}><PackageCheck size={15}/>交付</button>
        </div>
        <div className="mission-workbench-tools" role="toolbar" aria-label="任务工作区">
          <button type="button" className={`mission-context-tool${coordinationOpen?' is-active':''}`} aria-pressed={coordinationOpen} onClick={()=>{setFocusChange(null);setContextPanel(coordinationOpen?'none':'coordination');}}><MessageCircle size={15}/>协调助手</button>
          <button type="button" className={`mission-context-tool${detailsOpen?' is-active':''}`} aria-pressed={detailsOpen} onClick={()=>{setSelectedWorkId('');setContextPanel(detailsOpen?'none':'detail');}}><ClipboardList size={15}/>任务详情</button>
        </div>
      </div>

      <div className={`mission-desktop-workspace${coordinationOpen || detailsOpen ? ' has-context-panel' : ''}`} data-panel={coordinationOpen?'coordination':detailsOpen?'detail':'none'}>
      <div className="mission-workspace-main">
      {viewTab==='collaboration'?<div className="mission-collaboration-view" role="tabpanel" aria-label="协作">
        <section className="mission-work-section"><div className="mission-work-section-head"><h3>当前工作</h3><span>{actualConcurrency===null ? '并发状态待核实' : `${actualConcurrency} 项正在执行`}</span></div>
          {activeCards.length ? <div className="mission-work-grid">{activeCards.map(task=><MissionWorkCard key={task.spec.id} activities={relatedActivities} fresh={fresh} event={cardEvent(task.spec.id)} mission={mission} task={task as MissionView['tasks'][number]} onSelect={()=>openTask(task.spec.id)}/>)}</div> : !upcomingCards.length ? <div className="mission-work-empty">{completedCards.length ? '工作已交付，可在交付页查看成果和验收。' : '计划尚未生成，将按需求推荐专家团队。'}</div> : null}
        </section>
        {upcomingCards.length ? upcomingCards.length > 4 ? <details className="mission-upcoming-work"><summary>待派发与等待依赖 · {upcomingCards.length} 项</summary><div className="mission-work-grid">{upcomingCards.map(task=><MissionWorkCard key={task.spec.id} activities={relatedActivities} fresh={fresh} event={cardEvent(task.spec.id)} mission={mission} task={task as MissionView['tasks'][number]} onSelect={()=>openTask(task.spec.id)}/>)}</div></details>
          : <section className="mission-work-section"><div className="mission-work-section-head"><h3>下一步与交接</h3><span>等待中的工作不计入并发</span></div><div className="mission-work-grid">{upcomingCards.map(task=><MissionWorkCard key={task.spec.id} activities={relatedActivities} fresh={fresh} event={cardEvent(task.spec.id)} mission={mission} task={task as MissionView['tasks'][number]} onSelect={()=>openTask(task.spec.id)}/>)}</div></section> : null}
        {completedCards.length ? <details className="mission-completed-work"><summary>已完成的工作 · {completedCards.length} 项</summary><div className="mission-work-grid">{completedCards.map(task=><MissionWorkCard key={task.spec.id} activities={relatedActivities} fresh={fresh} event={cardEvent(task.spec.id)} mission={mission} task={task as MissionView['tasks'][number]} onSelect={()=>openTask(task.spec.id)}/>)}</div></details> : null}
      </div>:null}
      {viewTab==='delivery'?<div className="mission-delivery-view" role="tabpanel" aria-label="交付">
        <MissionEvidence mission={mission} events={events}/>
        <section className="mission-detail-panel"><h3>验收状态</h3><p>{verificationExplanation || (mission.status==='completed'?'当前交付已完成。':'当前交付尚未通过全部验收，可先查看已有证据。')}</p><p>{progress.done}/{progress.total || cardTasks.length} 项已通过 · {progress.awaitingVerification} 项待验收</p></section>
        {mission.status==='completed'?<details className="mission-workflow-save mission-optional-export"><summary>保存为可复用工作流</summary><p>保存后进入“我的能力”，移除本次路径、授权与私有记录。</p><CollaborationWorkflowExportPanel mission={mission}/></details>:null}
      </div>:null}
      </div>
      {coordinationOpen || detailsOpen ? <aside id="mission-context-panel" className="mission-workspace-aside" aria-label={coordinationOpen?'任务协调面板':'任务详情面板'}>
        <div className="mission-context-head"><strong>{coordinationOpen?'协调助手':'任务详情'}</strong><button type="button" className="btn btn-icon mission-context-close" title={coordinationOpen?'关闭协调助手':'关闭任务详情'} aria-label={coordinationOpen?'关闭协调助手':'关闭任务详情'} onClick={()=>{setContextPanel('none');}}><X size={16}/></button></div>
      {coordinationOpen ? <MissionCoordination message={messageDraft} onMessageChange={setMessageDraft} scrollPosition={conversationScroll} focusChange={focusChange} mission={mission} events={events} busy={messageBusy} onSend={sendCoordinationMessage} onApply={applyCoordinationChange} onWithdraw={async requestId=>{setMessageBusy(true);try{await agentApi.withdrawMissionInstruction(mission.mission_id,requestId);await poll(true);}catch(cause){setError(formatError(cause,'要求未撤回'));}finally{setMessageBusy(false);}}}/> : null}
      {!coordinationOpen && detailsOpen ? <section className="mission-secondary-panel">
        <div className="mission-detail-tabs segmented-control" role="tablist" aria-label="任务详细信息">{([{id:'overview',label:'运行概况'},{id:'plan',label:'计划与授权'},{id:'experts',label:'协作专家'},{id:'activity',label:'活动记录'}] as const).map(item=><button key={item.id} role="tab" aria-selected={tab===item.id} onClick={()=>setTab(item.id)}>{item.label}</button>)}</div>
        {tab==='overview'?<>      {!['failed', 'canceled'].includes(mission.status) ? <nav className={`mission-stage-rail${liveWork ? ' is-live' : ''}`} aria-label="协作任务进度">
        {flow.map((step, index) => <div key={step.label} className={`is-${step.state}${step.state === 'current' && currentStageIsVerification && !verificationStarted ? ' is-attention' : ''}${step.state === 'current' && liveWork ? ' is-running' : ''}`} aria-current={step.state === 'current' ? 'step' : undefined}>
          <span>{index < currentFlowIndex || mission.status === 'completed' ? <CheckCircle2 size={13} /> : index + 1}</span><strong>{step.label}</strong>
          {step.state === 'current' ? <small>{currentStageIsVerification ? (verificationRunLive ? '另一位专家核验中' : verificationStarted ? '核验任务已派发' : verificationQueued ? '等待系统安排核验' : '等待任务恢复') : liveWork ? '正在处理' : '当前阶段'}</small> : null}
        </div>)}
      </nav> : null}
      <div className="mission-detail-foot"><span>计划 {mission.plan_sha256 || '尚未生成'}</span><span>快照 {wait ? `${wait.contract_version} · ${wait.cursor}` : '关联未确认'}</span></div><MissionExecutionOverview mission={mission} events={events} activities={relatedActivities}/><MissionBudgetSummary mission={mission} events={events} historyComplete={historyComplete} now={now}/><dl className="task-facts"><div><dt>工作区</dt><dd>{mission.workspace.root || '未记录'}</dd></div><div><dt>来源</dt><dd>{clientLabel(mission.handoff?.source_client || '')}</dd></div><div><dt>当前阶段</dt><dd>{detailStageLabel}</dd></div><div><dt>更新时间</dt><dd>{new Date(mission.updated_at).toLocaleString()}</dd></div></dl></>:null}
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
            <h3>{mission.status === 'awaiting_confirmation' ? '待确认任务' : selectedWorkId ? '当前任务详情' : '执行子任务'}</h3>{selectedWorkId ? <button className="btn" onClick={()=>setSelectedWorkId('')}>查看全部任务</button> : null}
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
              {mission.tasks.filter(task => !selectedWorkId || task.spec.id === selectedWorkId).map(task => {
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
                      : '实现已提交；系统将运行必需检查，并安排专家独立核验。';
                const legacyOverride = legacyVerificationOverrideForTask(mission, events, task);
                const retryAvailable = canRetryWaitingVerification(mission, events, task);
                return (
                  <div className={`mission-task-row task-${task.status}`} key={task.spec.id} data-mission-task-id={task.spec.id}>
                    <span className="mission-task-status">{verificationStatus || missionTaskExecutionLabel(mission, task, relatedActivities)}</span>
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
                        ? <button className="btn" disabled={Boolean(rechecking)} title={legacyOverride ? '本次覆盖绑定指定暂停与失败事件，只重核同一交付' : '只重跑同一实现的必需检查与独立验收，不重做实现'} onClick={()=>void recheckTask(task.spec.id, task.verification_run_id, legacyOverride)}>{rechecking === task.spec.id ? '提交中' : legacyOverride ? '确认覆盖并重试核验' : mission.status === 'waiting' ? '重试核验并继续' : '重新核验同一实现'}</button> : null}
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
            ? '宿主自动派发独立验收；立即检查只核对交付，不重做。'
            : '继续原运行，避免重复执行。'}</p>
          <button type="button" className="btn" disabled={dispatching} onClick={() => void dispatchNext()}>
            {dispatching ? <BusyIndicator size={14} /> : <Activity size={14} />}
            {dispatching ? '检查中' : mission.tasks.some(task => task.status === 'awaiting_verification' && task.verification_run_id) ? '继续独立验收' : mission.tasks.some(task => task.status === 'awaiting_verification') ? '立即检查验收' : '恢复/检查运行'}
          </button>
        </div>
      ) : null}

        {mission.status === 'active' && !verificationRunLive && mission.tasks.some(task => task.status === 'ready') ? <button type="button" className="btn btn-primary" disabled={dispatching} onClick={() => void dispatchNext()}>{dispatching && liveWork ? <BusyIndicator size={14} /> : <Plus size={14} />}{dispatching ? '提交派发中' : mission.scheduling ? '检查可派发任务' : '继续执行下一项'}</button> : null}
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
      </section> : null}
      </aside> : null}
      </div>
      {assignmentOpen ? <CollaborationAssignmentDialog mission={mission} onClose={() => setAssignmentOpen(false)} onAssigned={next => { setMission(next); onMissionChanged?.(next); setAssignmentOpen(false); setTab('experts'); void poll(true); void onRefresh(); }} /> : null}
      {runtimeWorkspaceOpen ? <RuntimeWorkspaceDialog mission={mission} onClose={() => setRuntimeWorkspaceOpen(false)} onResolved={next => { setMission(next); onMissionChanged?.(next); setRuntimeWorkspaceOpen(false); void poll(true); void onRefresh(); }} /> : null}
      {budgetExtensionOpen ? <MissionBudgetExtensionDialog mission={mission} events={events} historyComplete={historyComplete} now={now} reason={wait?.budget_reason || ''} onClose={() => setBudgetExtensionOpen(false)} onUpdated={next => { setMission(next); onMissionChanged?.(next); setBudgetExtensionOpen(false); setNotice(next.status === 'waiting' ? '预算已更新；任务仍在等待，请按当前原因处理。' : '预算已更新；继续尚未完成的任务，交付结果仍需检查与独立验收。'); void poll(true); void onRefresh(); }} /> : null}

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
      <p className="mission-budget-reason">{reason || '仅追加所填额度；系统复核限制与环境，可能仍需等待。'}</p>
      <MissionBudgetSummary mission={mission} events={events} historyComplete={historyComplete} now={now}/>
      {saved ? <p role="status">有待确认的增额申请，可用原参数重试。<br />改参数会新增额度，请先刷新核对上次结果。</p> : null}
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
      <footer><small>{legacyOverrideCandidate ? '不会重做实现或增加返工次数。' : '续跑未完成任务并复核限额；响应丢失可按原参数重试。'}</small><button type="button" className="btn btn-primary" disabled={busy || budgetAction.disabled} onClick={() => void submit()}>{busy ? <BusyIndicator size={14} /> : <Plus size={14} />}{busy ? '更新中' : budgetAction.label}</button></footer>
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
  const inactiveRoles = useRef(new Map<string, MissionRoleAssignment>());

  useEffect(() => {
    let active = true;
    void agentApi.missionAssignmentOptions(mission.mission_id).then(next => {
      if (!active) return;
      setOptions(next);
      const initial = initialAssignmentRoles(mission, next);
      setRoles(initial);
      setTaskRoles(initialTaskRoles(mission, initial));
      setRecommended(mission.assignment?.recommended ?? true);
      inactiveRoles.current.clear();
    }).catch(cause => setError(formatError(cause, '指派选项读取失败')));
    return () => { active = false; };
  }, [mission.mission_id]);

  function useRecommended() {
    if (!options) return;
    setRecommended(roles.every(role => {
      const pin = options.roles.find(option => option.role_id === role.role_id)?.recommended_expert;
      return !!pin && role.expert?.expert_id === pin.expert_id && role.expert?.expert_version === pin.expert_version && role.expert?.expert_digest === pin.expert_digest;
    }));
    setRoles(current => current.map(role => {
      const option = options.roles.find(value => value.role_id === role.role_id);
      if (!option) return role;
      const defaults = recommendedRole(option, options);
      return { ...role, executor: defaults.executor, service_id: defaults.service_id, model: defaults.model };
    }));
  }

  function toggleRole(roleId: string) {
    const option = options?.roles.find(value => value.role_id === roleId);
    if (!options || !option?.optional) return;
    setRecommended(false);
    const existing = roles.find(role => role.role_id === roleId);
    if (existing) {
      inactiveRoles.current.set(roleId, existing);
      setRoles(current => current.filter(role => role.role_id !== roleId));
    } else {
      const restored = inactiveRoles.current.get(roleId) || recommendedRole(option, options);
      setRoles(current => [...current, restored]);
    }
  }

  async function submit() {
    if (!options || busy) return;
    const problems = roles.map(role => assignmentProblem(role, options)).filter(Boolean);
    const missingTaskRole = hasMissingTaskAssignment(mission, taskRoles, roles);
    if (problems.length) {
      setError(problems[0]);
      return;
    }
    if (missingTaskRole) {
      setError('请为每项计划任务选择已启用且有指派的职责');
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
  const missingTaskRole = hasMissingTaskAssignment(mission, taskRoles, roles);

  return (
    <CollaborationDialog titleId="mission-assignment-title" onClose={onClose}>
      <section className="collaboration-create-dialog collaboration-assignment-dialog" aria-labelledby="mission-assignment-title">
        <header>
          <div><span>需求对应的协作职责</span><h2 id="mission-assignment-title">选择协作专家</h2><small>{options ? `${options.workflow_id}@${options.workflow_version}` : '正在读取职责与推荐'}</small></div>
          <button type="button" className="btn" onClick={onClose}>关闭</button>
        </header>
        <p>核对推荐专家与配置，应用后确认计划和访问范围。</p>
        {options?.recommendation_warnings?.length ? <div role="status"><strong>推荐说明与局限</strong><ul>{options.recommendation_warnings.map((warning, index) => <li key={index}>{warning}</li>)}</ul></div> : null}
        {error ? <p className="error" role="alert">{error}</p> : null}
        {options ? <div className="collaboration-assignment-list">{options.roles.map(option => {
          const assigned = roles.find(role => role.role_id === option.role_id);
          const enabled = !!assigned;
          const value = assigned || inactiveRoles.current.get(option.role_id) || recommendedRole(option, options);
          const selectedExpert = value.expert ? options.experts.find(expert => expert.id === value.expert?.expert_id && expert.version === value.expert?.expert_version && expert.digest === value.expert?.expert_digest) : undefined;
          const selectedExpertKey = value.expert ? `${value.expert.expert_id}@${value.expert.expert_version}:${value.expert.expert_digest}` : '';
          const selectedExecutor = options.executors.find(executor => executor.executor === value.executor);
          const services = serviceOptions(selectedExecutor);
          const models = (selectedExecutor?.models || []).filter(model => model.service_id === value.service_id);
          const problem = enabled ? assignmentProblem(value, options) : '';
          return <article className="collaboration-assignment-role" key={option.role_id}>
            <div className="collaboration-assignment-role-heading"><strong>{option.label || option.role_id}{option.optional ? '（可选）' : ''}</strong><small>{option.brief || option.role_id}</small>{option.recommendation_reason ? <small>推荐理由：{option.recommendation_reason}</small> : <small>暂无专家推荐，请手动选择。</small>}<em className={`collaboration-assignment-availability is-${option.availability}`}>{option.availability === 'available' ? '执行配置可用' : option.unavailable_reason || '推荐配置不可用，可更换执行器'}</em>{option.optional ? <button type="button" className="btn" role="switch" aria-label={`${option.label || option.role_id}职责`} aria-checked={enabled} onClick={() => toggleRole(option.role_id)}>{enabled ? '已启用' : '未启用'}</button> : null}{problem ? <em className="collaboration-assignment-problem" role="status">{problem}</em> : null}</div>
            <label><span>专家 *</span><select aria-label={`${option.label || option.role_id}专家`} value={selectedExpertKey} disabled={!enabled} onChange={event => { setRecommended(false); const expert = options.experts.find(item => expertKey(item) === event.target.value); setRoles(current => current.map(role => role.role_id === option.role_id ? bindExpert(role, expert) : role)); }}><option value="">请选择已安装专家</option>{value.expert && !selectedExpert ? <option value={selectedExpertKey} disabled>所选专家已不在目录，请重新选择</option> : null}{options.experts.map(expert => <option key={expertKey(expert)} value={expertKey(expert)}>{expert.name} · {expert.version}</option>)}</select><small>{selectedExpert ? `摘要 ${selectedExpert.digest.slice(0, 12)}…` : enabled ? '提交前必须选择专家版本' : '启用此职责后可选择专家'}</small></label>
            <label><span>执行器 *</span><select aria-label={`${option.label || option.role_id}执行器`} value={value.executor} disabled={!enabled} onChange={event => { const executor = options.executors.find(item => item.executor === event.target.value); if (!executor) return; setRecommended(false); setRoles(current => current.map(role => role.role_id === option.role_id ? changeExecutor(role, executor) : role)); }}><option value="">请选择宿主执行器</option>{options.executors.map(executor => <option key={executor.executor} value={executor.executor} disabled={!executor.available}>{executor.label}{executor.available ? '' : ` · ${executor.reason || '不可用'}`}</option>)}</select></label>
            <label><span>服务 *</span><select aria-label={`${option.label || option.role_id}服务`} value={value.service_id} disabled={!enabled || !selectedExecutor || selectedExecutor.models.length === 0} onChange={event => { setRecommended(false); setRoles(current => current.map(role => role.role_id === option.role_id ? changeService(role, selectedExecutor, event.target.value) : role)); }}><option value="">{services.length ? '请选择服务' : '由执行器自身配置'}</option>{services.map(service => <option key={service.id} value={service.id}>{service.label || service.id}</option>)}</select></label>
            <label><span>模型 *</span><select aria-label={`${option.label || option.role_id}模型`} value={value.model} disabled={!enabled || !selectedExecutor || selectedExecutor.models.length === 0 || !value.service_id} onChange={event => { setRecommended(false); setRoles(current => current.map(role => role.role_id === option.role_id ? { ...role, model: event.target.value } : role)); }}><option value="">{models.length ? '请选择模型' : '由执行器自身配置'}</option>{models.map(model => <option key={model.alias} value={model.alias}>{model.label || model.alias}</option>)}</select></label>
          </article>;
        })}</div> : <div className="collaboration-task-loading"><BusyIndicator size={16} />正在读取职责目录</div>}
        {mission.proposed_tasks.length ? <div className="collaboration-task-role-map"><strong>任务与职责映射 *</strong>{mission.proposed_tasks.map(task => <label key={task.id}><span>{task.title}</span><select aria-label={`${task.title}承担专家`} value={taskRoles[task.id] || ''} onChange={event => { setRecommended(false); setTaskRoles(current => ({ ...current, [task.id]: event.target.value })); }}><option value="">请选择承担专家</option>{taskRoles[task.id] && !roles.some(role => role.role_id === taskRoles[task.id]) ? <option value={taskRoles[task.id]} disabled>原职责未启用，请重新选择</option> : null}{roles.map(role => <option key={role.role_id} value={role.role_id}>{role.label || role.role_id}</option>)}</select></label>)}</div> : null}
        {missingTaskRole ? <p className="error" role="status">请为每项计划任务选择已启用且有指派的职责。</p> : null}
        <footer className="collaboration-assignment-actions"><small>推荐按钮仅填入配置，不会启动；专家版本与摘要随指派固定。</small><button type="button" className="btn" disabled={!options || busy} onClick={useRecommended}>使用推荐服务与模型</button><button type="button" className="btn btn-primary" disabled={!options || busy || roles.length === 0 || problems.length > 0 || missingTaskRole} onClick={() => void submit()}>{busy ? <BusyIndicator size={14} /> : <ShieldCheck size={14} />}{busy ? '应用中' : '应用角色指派'}</button></footer>
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
      const saved = await agentApi.saveMissionWorkflow(mission.mission_id, workflowId.trim(), version.trim(), name.trim(), 'start');
      setNotice(`${saved.package.name} · ${saved.package.version} 已保存到“我的能力”，创建新任务时可选择此工作流。`);
    } catch (cause) { setError(formatError(cause, '协作工作流导出失败')); }
    finally { setBusy(false); }
  }
  return <section className="mission-detail-panel collaboration-export-panel" aria-labelledby="mission-export-title">
    <div className="mission-detail-panel-head"><div><h3 id="mission-export-title">沉淀为协作工作流</h3><small>只提取已验证的角色、步骤与验收；不会发布</small></div><Download size={16} aria-hidden="true" /></div>
    <p>仅保留可复用角色、步骤和验收。</p>
    <p>本次路径、对话、运行记录与用量不会导出。</p>
    <p>保存后可在新工作区复用，每次执行重新确认访问范围。</p>
    {error ? <p className="error" role="alert">{error}</p> : null}
    {notice ? <p className="collaboration-export-notice" role="status">{notice}</p> : null}
    <div className="collaboration-export-fields">
      <label><span>工作流 ID</span><input value={workflowId} onChange={event => setWorkflowId(event.target.value)} /></label>
      <label><span>版本</span><input value={version} onChange={event => setVersion(event.target.value)} /></label>
      <label><span>名称</span><input value={name} onChange={event => setName(event.target.value)} /></label>
      <button type="button" className="btn" disabled={busy || !workflowId.trim() || !version.trim() || !name.trim()} onClick={() => void exportWorkflow()}>{busy ? <BusyIndicator size={14} /> : <Download size={14} />}{busy ? '保存中' : '保存到我的能力'}</button>
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
