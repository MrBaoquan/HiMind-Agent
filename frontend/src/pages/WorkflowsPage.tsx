import { useEffect, useMemo, useRef, useState } from 'react';
import { ArrowDown, Boxes, CalendarClock, CheckCircle2, CircleAlert, Clock3, FileText, FolderOpen, GitBranch, KeyRound, LoaderCircle, MessageCircle, Play, Power, RefreshCw, Repeat2, RotateCcw, ShieldCheck, Store, Trash2, Upload, Workflow, X } from 'lucide-react';
import { EmptyState, PageHeader, Pill } from '../components/Common';
import type { WorkflowCenterSnapshot, WorkflowLocalRun, WorkflowPreflight, WorkflowRunPreset, WorkflowRunPresetInput, WorkflowRunSnapshot, WorkflowRunVerification, WorkflowStep } from '../services/agentApi';
import {
  WorkflowStartFieldError,
  fieldsToInput,
  initialFormValues,
  initialFieldValue,
  invalidOptionValue,
  jsonToFormValues,
  normalizeStartField,
  usesFullRow,
  type WorkflowStartField,
} from './workflowStartForm';
import {
  buildRunActivity,
  buildRunTimeline,
  formatClock,
  formatElapsedCn,
  formatIdleCn,
  toEpochSeconds,
  type RunStepState,
} from './workflowRunView';
import { buildWorkflowGraph } from './workflowGraph';

type WorkflowsPageProps = {
  snapshot: WorkflowCenterSnapshot | null;
  /** 从待处理中心或定时任务跳入时，直接打开指定 Run。 */
  initialRunId?: string;
  loading: boolean;
  error: string;
  onRefresh: () => void;
  onLoadRun: (runId: string) => Promise<WorkflowRunSnapshot>;
  onVerify: (runId: string) => Promise<WorkflowRunVerification>;
  onRevealArtifact: (runId: string, artifactId: string) => Promise<void>;
  onApprove: (runId: string, stepId: string) => Promise<void>;
  onReject: (runId: string, stepId: string) => Promise<void>;
  onResume: (runId: string, feedback: string) => Promise<void>;
  onCancel: (runId: string) => Promise<void>;
  onStart: (packageId: string, input: Record<string, unknown>) => Promise<WorkflowLocalRun>;
  onPreflight: (packageId: string, input: Record<string, unknown>) => Promise<WorkflowPreflight>;
  onSaveCredentialFile: (connectorId: string, handle: string) => Promise<boolean>;
  onSaveCredentialSecret: (connectorId: string, handle: string, secret: string) => Promise<void>;
  onInstallLocal: () => Promise<void>;
  onSetEnabled: (packageId: string, enabled: boolean) => Promise<void>;
  onRollback: (packageId: string) => Promise<void>;
  onRemove: (packageId: string) => Promise<void>;
  onPickDirectory: () => Promise<string | null>;
  onOpenExtensions: () => void;
  /** 跳到平台级「定时任务」页，并预选该工作流作为定时目标。 */
  onScheduleWorkflow: (workflowId: string) => void;
  onLoadPresets: (workflowId: string) => Promise<{ presets: WorkflowRunPreset[] }>;
  onSavePreset: (input: WorkflowRunPresetInput) => Promise<void>;
  onDeletePreset: (id: string) => Promise<void>;
};

function statusKind(status: string): 'success' | 'warn' | 'danger' | 'neutral' {
  if (status === 'succeeded') return 'success';
  if (status === 'failed' || status === 'canceled') return 'danger';
  if (status === 'waiting' || status === 'running') return 'warn';
  return 'neutral';
}

function statusLabel(status: string) {
  return ({
    queued: '排队中',
    running: '执行中',
    waiting: '等待操作',
    succeeded: '已完成',
    failed: '失败',
    canceled: '已取消',
    pending: '待执行',
    skipped: '已跳过',
  } as Record<string, string>)[status] || status;
}

function formatTime(value: string) {
  if (!value) return '--';
  const numeric = Number(value);
  const date = Number.isFinite(numeric) ? new Date(numeric * 1000) : new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

function formatDuration(seconds: number) {
  if (!Number.isFinite(seconds) || seconds <= 0) return '--';
  if (seconds < 60) return `${Math.round(seconds)} 秒`;
  if (seconds < 3600) return `${Math.round(seconds / 60)} 分`;
  return `${(seconds / 3600).toFixed(1)} 小时`;
}

function formatBytes(bytes: number) {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(unit ? 1 : 0)} ${units[unit]}`;
}

function waitingKindLabel(kind: string) {
  if (kind === 'feedback') return '等待反馈';
  if (kind === 'approval') return '等待审批';
  if (kind === 'form') return '等待填写';
  if (kind === 'evidence') return '等待证据';
  if (kind === 'external_wait') return '等待外部状态';
  return '等待处理';
}

function interactionActionLabel(action: string) {
  return ({
    approve_or_reject: '需要批准或拒绝',
    submit_feedback: '需要提交反馈',
    submit_form: '需要填写表单',
    submit_evidence: '需要提交证据',
    inspect_run: '需要检查运行状态',
  } as Record<string, string>)[action] || action;
}

function executionPolicyLabel(policy?: string) {
  return ({ strict: '严格执行', segmented: '分段执行', flexible: '灵活执行' } as Record<string, string>)[policy || 'strict'] || '自定义策略';
}

function stepKindLabel(kind?: string) {
  return ({ capability: '自动步骤', runtime: 'AI 步骤', manual: '人工步骤', loop: '循环步骤', provider_defined: '扩展步骤' } as Record<string, string>)[kind || ''] || '工作流步骤';
}

function runtimeProviderLabel(provider?: string) {
  if (!provider) return '自动选择';
  if (provider === 'himind.builtin' || provider.includes('deepseek')) return 'HiMind AI';
  if (provider.includes('codex')) return 'Codex';
  if (provider.includes('copilot')) return 'GitHub Copilot';
  return '自定义运行环境';
}

function riskLevelLabel(risk?: string) {
  return ({ R1: '低', R2: '中', R3: '高', R4: '最高' } as Record<string, string>)[risk || 'R3'] || '高';
}

// 时间线用的是归一化后的步骤状态，标签与配色都从这里出，避免各处各写一份。
function runStepStateLabel(state: RunStepState) {
  return ({
    done: '已完成',
    active: '进行中',
    waiting: '等待处理',
    failed: '失败',
    canceled: '已取消',
    skipped: '已跳过',
    pending: '待执行',
  } as Record<RunStepState, string>)[state] || state;
}

function runStepStateKind(state: RunStepState): 'success' | 'warn' | 'danger' | 'neutral' {
  if (state === 'done') return 'success';
  if (state === 'failed' || state === 'canceled') return 'danger';
  if (state === 'active' || state === 'waiting') return 'warn';
  return 'neutral';
}

function runElapsedSeconds(run: WorkflowLocalRun) {
  const created = Number(run.created_at);
  const updated = Number(run.updated_at);
  if (Number.isFinite(created) && Number.isFinite(updated) && updated >= created) return updated - created;
  const start = new Date(run.created_at).getTime();
  const end = new Date(run.updated_at).getTime();
  return Number.isFinite(start) && Number.isFinite(end) && end >= start ? (end - start) / 1000 : 0;
}

function currentFeedbackQuestion(snapshot: WorkflowRunSnapshot) {
  if (snapshot.interaction_request?.kind === 'feedback' && snapshot.interaction_request.description.trim()) {
    return snapshot.interaction_request.description;
  }
  const event = [...snapshot.events].reverse().find(item => item.event_type === 'question_requested');
  if (!event || typeof event.payload !== 'object' || event.payload === null) return '';
  const payload = event.payload as Record<string, unknown>;
  for (const key of ['question', 'message', 'prompt', 'reason']) {
    if (typeof payload[key] === 'string' && String(payload[key]).trim()) return String(payload[key]);
  }
  return '';
}

function approvalDescription(step: WorkflowStep | undefined, _run: WorkflowLocalRun, request?: WorkflowRunSnapshot['interaction_request']) {
  if (request?.description?.trim()) {
    return [request.description.trim(), request.required_action ? interactionActionLabel(request.required_action) : ''].filter(Boolean).join(' · ');
  }
  if (!step) return '请确认当前操作后继续。';
  const risk = typeof step.risk_level === 'string' ? step.risk_level : 'R3';
  return `“${step.title || '当前步骤'}”需要你的确认 · ${riskLevelLabel(risk)}风险`;
}

function PreflightPanel({
  report,
  busy,
  uploadChannel,
  onSaveFile,
  onSaveSecret,
}: {
  report: WorkflowPreflight;
  busy: boolean;
  uploadChannel: string;
  onSaveFile: (connectorId: string, handle: string) => Promise<boolean>;
  onSaveSecret: (connectorId: string, handle: string, secret: string) => Promise<void>;
}) {
  const [secretDrafts, setSecretDrafts] = useState<Record<string, string>>({});
  const [credentialBusy, setCredentialBusy] = useState('');
  const [credentialError, setCredentialError] = useState('');
  const missingSkills = report.skills.filter(item => !item.available).map(item => item.id);
  const missingRuntimes = report.runtimes.filter(item => !item.available).map(item => {
    if (item.status === 'ready' && !item.network_isolated) return `${item.id}（未隔离网络）`;
    return item.id;
  });
  const missingTools = report.tools.filter(item => item.required && !item.available).map(item => item.id);
  const diagnostics = report.diagnostics?.length
    ? report.diagnostics
    : report.blockers.map(message => ({ severity: 'blocker', code: 'legacy.blocker', stage: 'preflight', message, remediation: '根据提示修复后重新执行启动前检查。', retryable: true }))
      .concat(report.warnings.map(message => ({ severity: 'warning', code: 'legacy.warning', stage: 'preflight', message, remediation: '根据提示处理；该项当前不阻断启动。', retryable: true })));
  const blockerDiagnostics = diagnostics.filter(item => item.severity === 'blocker');
  const warningDiagnostics = diagnostics.filter(item => item.severity === 'warning');
  const missingCredentials = report.connectors.flatMap(connector =>
    connector.credentials
      .filter(credential => !credential.configured && (
        credential.required
        || (uploadChannel === 'ci' && credential.target === 'private_key_path')
      ))
      .map(credential => ({ ...credential, connectorId: connector.id })),
  );

  async function configureFile(connectorId: string, handle: string) {
    setCredentialBusy(`file:${handle}`);
    setCredentialError('');
    try {
      await onSaveFile(connectorId, handle);
    } catch (error) {
      setCredentialError(error instanceof Error ? error.message : '保存凭据文件失败');
    } finally {
      setCredentialBusy('');
    }
  }

  async function configureSecret(connectorId: string, handle: string) {
    const secret = (secretDrafts[handle] || '').trim();
    if (!secret) return;
    setCredentialBusy(`secret:${handle}`);
    setCredentialError('');
    try {
      await onSaveSecret(connectorId, handle, secret);
      setSecretDrafts(current => ({ ...current, [handle]: '' }));
    } catch (error) {
      setCredentialError(error instanceof Error ? error.message : '保存连接密钥失败');
    } finally {
      setCredentialBusy('');
    }
  }

  return <section className={`workflow-preflight ${report.ready ? 'ready' : 'blocked'}`}>
    <div className="workflow-preflight-head">
      {report.ready ? <CheckCircle2 size={17} /> : <CircleAlert size={17} />}
       <span><strong>{report.ready ? '启动前检查通过' : '启动前检查未通过'}</strong><small>{report.ready ? '依赖和运行环境已就绪' : blockerDiagnostics.map(item => item.message).join('；')}</small></span>
    </div>
    <div className="workflow-preflight-grid">
      <span>功能 {report.capabilities.filter(item => item.available).length}/{report.capabilities.length}</span>
      <span>技能 {report.skills.filter(item => item.available).length}/{report.skills.length}</span>
      <span>运行环境 {report.runtimes.filter(item => item.available).length}/{report.runtimes.length}</span>
      <span>连接 {report.connectors.filter(item => item.available && item.health_status === 'passed').length}/{report.connectors.length}</span>
    </div>
    {missingCredentials.length ? <div className="workflow-preflight-credentials">
      <div className="workflow-preflight-credentials-head"><KeyRound size={15} /><span><strong>需要配置凭据</strong><small>保存后会自动重新检查，不需要重新打开此窗口。</small></span></div>
      {missingCredentials.map(credential => (
        <div key={credential.handle} className="workflow-preflight-credential">
          <span>
            <strong>{credential.handle}</strong>
            <small>{credential.connectorId} · {credential.kind === 'file_path' ? '文件凭据' : '密钥'} · {credential.target}</small>
          </span>
          {credential.kind === 'file_path' ? (
            <button type="button" className="btn" disabled={busy || Boolean(credentialBusy)} onClick={() => void configureFile(credential.connectorId, credential.handle)}>
              {credentialBusy === `file:${credential.handle}` ? <LoaderCircle className="spin" size={13} /> : <FolderOpen size={13} />}
              {credentialBusy === `file:${credential.handle}` ? '保存中' : '选择文件'}
            </button>
          ) : (
            <div className="workflow-preflight-secret">
              <input
                type="password"
                value={secretDrafts[credential.handle] || ''}
                placeholder="输入 Secret"
                autoComplete="new-password"
                onChange={event => setSecretDrafts(current => ({ ...current, [credential.handle]: event.target.value }))}
              />
              <button type="button" className="btn" disabled={busy || Boolean(credentialBusy) || !(secretDrafts[credential.handle] || '').trim()} onClick={() => void configureSecret(credential.connectorId, credential.handle)}>
                {credentialBusy === `secret:${credential.handle}` ? <LoaderCircle className="spin" size={13} /> : <CheckCircle2 size={13} />}
                {credentialBusy === `secret:${credential.handle}` ? '保存中' : '保存'}
              </button>
            </div>
          )}
        </div>
      ))}
      {credentialError ? <div className="workflow-preflight-credential-error">{credentialError}</div> : null}
    </div> : null}
    {missingSkills.length || missingRuntimes.length || missingTools.length ? <div className="workflow-preflight-missing">
      {missingSkills.length ? <span>缺少技能：{missingSkills.join('、')}</span> : null}
      {missingRuntimes.length ? <span>运行环境不可用：{missingRuntimes.join('、')}</span> : null}
      {missingTools.length ? <span>缺少工具：{missingTools.join('、')}</span> : null}
    </div> : null}
    {blockerDiagnostics.length ? <details open><summary>{blockerDiagnostics.length} 个阻塞项</summary>{blockerDiagnostics.map(item => <p key={`${item.code}:${item.message}`}><strong>{item.code}</strong> · {item.message}<br /><small>{item.remediation}{item.retryable ? ' 修复后可重试。' : ''}</small></p>)}</details> : null}
    {warningDiagnostics.length ? <details><summary>{warningDiagnostics.length} 条非阻塞提示</summary>{warningDiagnostics.map(item => <p key={`${item.code}:${item.message}`}><strong>{item.code}</strong> · {item.message}<br /><small>{item.remediation}</small></p>)}</details> : null}
  </section>;
}

export function WorkflowsPage({ snapshot, initialRunId = '', loading, error, onRefresh, onLoadRun, onVerify, onRevealArtifact, onApprove, onReject, onResume, onCancel, onStart, onPreflight, onSaveCredentialFile, onSaveCredentialSecret, onInstallLocal, onSetEnabled, onRollback, onRemove, onPickDirectory, onOpenExtensions, onScheduleWorkflow, onLoadPresets, onSavePreset, onDeletePreset }: WorkflowsPageProps) {
  const [view, setView] = useState<'runs' | 'library'>('runs');
  const [selectedWorkflowId, setSelectedWorkflowId] = useState('');
  const [selectedRunId, setSelectedRunId] = useState('');
  const [runDetail, setRunDetail] = useState<WorkflowRunSnapshot | null>(null);
  const [runVerification, setRunVerification] = useState<WorkflowRunVerification | null>(null);
  const [runLoading, setRunLoading] = useState(false);
  const [runError, setRunError] = useState('');
  const [actionBusy, setActionBusy] = useState('');
  const [resumeFeedback, setResumeFeedback] = useState('');
  const [startOpen, setStartOpen] = useState(false);
  const [startInput, setStartInput] = useState('');
  const [startError, setStartError] = useState('');
  const [startFieldErrors, setStartFieldErrors] = useState<Record<string, string>>({});
  const startFormRef = useRef<HTMLDivElement | null>(null);
  // 刚刚从弹窗启动的那次运行：用来在详情里给出「已提交、正在执行」的即时反馈。
  const [startedRunId, setStartedRunId] = useState('');
  // 提交回执：点下「启动运行」后弹窗要给出可见的受理结果，而不是直接消失。
  const [startReceipt, setStartReceipt] = useState('');
  const [startBusy, setStartBusy] = useState(false);
  // 运行详情的「现在」：每秒跳一次，时长与「最近更新」才是活的，而不是等刷新才变。
  const [nowMs, setNowMs] = useState(() => Date.now());
  const [localInstallBusy, setLocalInstallBusy] = useState(false);
  const [packageBusy, setPackageBusy] = useState('');
  const [startMode, setStartMode] = useState<'form' | 'json'>('form');
  const [startForm, setStartForm] = useState<Record<string, unknown>>({});
  const [preflight, setPreflight] = useState<WorkflowPreflight | null>(null);
  const [preflightBusy, setPreflightBusy] = useState(false);
  const [runWorkflowFilter, setRunWorkflowFilter] = useState('all');
  const [runStatusFilter, setRunStatusFilter] = useState('all');
  const [presets, setPresets] = useState<WorkflowRunPreset[]>([]);
  const [presetBusy, setPresetBusy] = useState('');
  const [presetLabel, setPresetLabel] = useState('');
  const [presetError, setPresetError] = useState('');
  const workflows = snapshot?.workflows || [];
  const runs = snapshot?.runs || [];
  const selectedWorkflow = useMemo(
    () => workflows.find(item => item.package.id === selectedWorkflowId) || workflows[0] || null,
    [selectedWorkflowId, workflows],
  );
  const selectedWorkflowGraph = useMemo(() => (selectedWorkflow ? buildWorkflowGraph(selectedWorkflow.package) : null), [selectedWorkflow]);

  useEffect(() => {
    if (!selectedWorkflowId && workflows[0]) setSelectedWorkflowId(workflows[0].package.id);
  }, [selectedWorkflowId, workflows]);

  useEffect(() => {
    if (view !== 'library') return;
    void loadPresets(selectedWorkflow?.package.id || '');
  }, [view, selectedWorkflow?.package.id]);

  async function openRun(runId: string) {
    setSelectedRunId(runId);
    setRunVerification(null);
    setResumeFeedback('');
    setRunLoading(true);
    setRunError('');
    try {
      setRunDetail(await onLoadRun(runId));
    } catch {
      setRunDetail(null);
      setRunError('运行详情读取失败');
    } finally {
      setRunLoading(false);
    }
  }

  async function performRunAction(runId: string, action: string, operation: () => Promise<void>) {
    setActionBusy(action);
    setRunError('');
    try {
      await operation();
      await openRun(runId);
    } catch {
      setRunError('工作流操作失败，请查看运行日志后重试');
    } finally {
      setActionBusy('');
    }
  }

  async function performPackageAction(action: string, operation: () => Promise<void>) {
    setPackageBusy(action);
    try {
      await operation();
      await onRefresh();
    } finally {
      setPackageBusy('');
    }
  }

  useEffect(() => {
    if (!initialRunId) return;
    setView('runs');
    void openRun(initialRunId);
  }, [initialRunId]);

  async function verifyRun(runId: string) {
    setActionBusy('verify');
    setRunError('');
    try {
      setRunVerification(await onVerify(runId));
    } catch {
      setRunVerification(null);
  setRunError('运行验证失败，版本或输出文件信息不完整');
    } finally {
      setActionBusy('');
    }
  }

  const waitingRequest = runDetail?.interaction_request || null;
  // 新版本优先消费后端稳定投影；旧 Ledger 没有该字段时再用历史事件兼容。
  const waitingForFeedback = Boolean(runDetail && runDetail.run.status === 'waiting' && (
    waitingRequest?.kind === 'feedback'
    || (!waitingRequest && runDetail.events.some(event =>
      event.step_id === runDetail.run.current_step_id
      && typeof event.payload === 'object'
      && event.payload !== null
      && (
        event.event_type === 'question_requested'
        || (event.payload as { waiting_for_feedback?: boolean }).waiting_for_feedback === true
      ),
    ))
  ));
  const waitingForApproval = Boolean(runDetail && runDetail.run.status === 'waiting' && (
    waitingRequest?.kind === 'approval'
    || (!waitingRequest && !waitingForFeedback && Boolean(runDetail.run.current_step_id))
  ));
  const waitingForExternal = Boolean(runDetail && runDetail.run.status === 'waiting' && waitingRequest?.kind === 'external_wait');
  const waitingForOtherInteraction = Boolean(runDetail && runDetail.run.status === 'waiting' && waitingRequest && !waitingForFeedback && !waitingForApproval);
  const currentWaitingKind = waitingRequest?.kind || (waitingForFeedback ? 'feedback' : waitingForApproval ? 'approval' : 'external_wait');
  const orderedRuns = useMemo(() => [...runs].sort((left, right) => {
    // 需要人处理的排最前，其次是在跑的；历史结果（含失败）按时间倒序，
    // 避免一批陈旧的失败运行长期占据列表顶部、挡住最新一次成功运行。
    const priority = (item: typeof left) => item.run.status === 'waiting' ? 0 : item.run.status === 'running' ? 1 : 2;
    return priority(left) - priority(right) || right.run.updated_at.localeCompare(left.run.updated_at);
  }), [runs]);
  const filteredRuns = useMemo(() => orderedRuns.filter(item => {
    if (runWorkflowFilter !== 'all' && item.workflow_id !== runWorkflowFilter) return false;
    if (runStatusFilter !== 'all' && item.run.status !== runStatusFilter) return false;
    return true;
  }), [orderedRuns, runStatusFilter, runWorkflowFilter]);
  const currentStepDefinition = useMemo(() => {
    const stepId = runDetail?.run.current_step_id || '';
    return runDetail?.workflow?.package.steps.find(step => step.id === stepId);
  }, [runDetail]);
  // 实际用到的模型与服务来源：从 Runtime 步骤输出里读，不在 UI 侧猜配置。
  const runtimeFacts = useMemo(() => {
    const events = runDetail?.events || [];
    for (const event of [...events].reverse()) {
      const output = (event.payload as { output?: Record<string, unknown> } | null)?.output;
      const model = output?.model;
      if (typeof model === 'string' && model) {
        return {
          model,
          provider: typeof output?.provider === 'string' ? output.provider : '',
          service: typeof output?.service_source === 'string' ? output.service_source : '',
          endpoint: typeof output?.endpoint === 'string' ? output.endpoint : '',
          stepId: event.step_id,
        };
      }
    }
    return null;
  }, [runDetail]);
  const serviceLabel = (source: string) => ({
    managed: '平台托管',
    custom: '本机自定义服务',
    native: '内置配置',
  } as Record<string, string>)[source] || source;
  // 区分“这次压根没调用 AI”和“调用了但没成功”：后者不该被说成没调用。
  const hasRuntimeStep = Boolean(runDetail?.workflow?.package.steps.some(step => step.kind === 'runtime'));
  const pendingCount = runs.filter(item => item.run.status === 'waiting').length;
  const runningCount = runs.filter(item => item.run.status === 'running' || item.run.status === 'queued').length;
  const failedCount = runs.filter(item => item.run.status === 'failed').length;

  useEffect(() => {
    if (view === 'runs' && !selectedRunId && orderedRuns[0]) {
      void openRun(orderedRuns[0].run.run_id);
    }
  }, [orderedRuns, selectedRunId, view]);

  useEffect(() => {
    const active = runDetail && ['queued', 'running', 'waiting'].includes(runDetail.run.status);
    if (!selectedRunId || !active) return;
    let disposed = false;
    const refreshDetail = async () => {
      if (disposed || document.visibilityState === 'hidden') return;
      try {
        const next = await onLoadRun(selectedRunId);
        if (!disposed) setRunDetail(next);
      } catch {
        // The list-level poll remains authoritative; keep the last detail
        // snapshot visible when a transient detail read fails.
      }
    };
    const timer = window.setInterval(() => void refreshDetail(), 5000);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, [onLoadRun, runDetail?.run.status, selectedRunId]);

  // 只要界面上有活动运行就按秒推进「现在」：列表里的相对更新时间和详情里的
  // 计时都会跟着动，用户看到的是活的进度而不是一张静止的快照。
  useEffect(() => {
    const hasActiveRun = runs.some(item => ['queued', 'running', 'waiting'].includes(item.run.status));
    if (!hasActiveRun) return;
    setNowMs(Date.now());
    const timer = window.setInterval(() => setNowMs(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [runs]);

  const timeline = useMemo(() => (runDetail ? buildRunTimeline(runDetail, nowMs) : null), [runDetail, nowMs]);
  const activity = useMemo(() => (runDetail ? buildRunActivity(runDetail) : []), [runDetail]);

  function openStart(prefill?: Record<string, unknown>) {
    if (!selectedWorkflow) return;
    const fields = normalizedStartFields(selectedWorkflow);
    // 预设只覆盖它带有的字段，其余仍走工作流声明的默认值；
    // 列表必须经过同一条 JSON→表单转换，否则数组会被拍成一行 "cv,llm,ar-vr"。
    const values = initialFormValues(fields, prefill);
    setStartForm(values);
    setStartInput(JSON.stringify(
      prefill ? { ...fieldsToInput(fields, values), ...prefill } : fieldsToInput(fields, values),
      null,
      2,
    ));
    setStartMode('form');
    setStartError('');
    setStartFieldErrors({});
    setPreflight(null);
    setStartOpen(true);
  }

  function normalizedStartFields(workflow: NonNullable<typeof selectedWorkflow>) {
    return (workflow.view?.sections || []).flatMap(section => (section.fields || []).map(normalizeStartField));
  }

  function switchStartMode(mode: 'form' | 'json') {
    if (mode === startMode) return;
    const fields = normalizedStartFields(selectedWorkflow!);
    if (mode === 'json') {
      try {
        setStartInput(JSON.stringify(fieldsToInput(fields, startForm), null, 2));
        setStartError('');
        setStartFieldErrors({});
        setStartMode(mode);
      } catch {
        setStartError('结构化字段暂时无法转换为 JSON，请检查列表或 JSON 字段');
      }
      return;
    }
    try {
      const parsed = JSON.parse(startInput);
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('not object');
      setStartForm(jsonToFormValues(fields, parsed));
      setStartError('');
      setStartFieldErrors({});
      setStartMode(mode);
    } catch {
      setStartError('JSON 需要是有效对象，才能切换回表单');
    }
  }

  function buildStartInput(): Record<string, unknown> {
    if (!selectedWorkflow) throw new Error('missing workflow');
    if (startMode === 'form') {
      const fields = normalizedStartFields(selectedWorkflow);
      const missing = fields.find(field => field.required && !String(startForm[field.id] ?? '').trim());
      if (missing) throw new WorkflowStartFieldError(missing);
      // 声明了 options 的字段就是「允许取值集合」：列表字段按行比对，下拉直接比对。
      // 在本地先拦住，用户看到的是字段级提示，而不是一次白跑的执行。
      const invalid = fields
        .map(field => ({ field, invalidValue: invalidOptionValue(field, startForm[field.id]) }))
        .find(item => item.invalidValue);
      if (invalid) {
        throw new WorkflowStartFieldError(
          invalid.field,
          `${invalid.field.label}不支持「${invalid.invalidValue}」，可选：${invalid.field.options.join('、')}`,
        );
      }
      return fieldsToInput(fields, startForm);
    }
    const parsed = JSON.parse(startInput);
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
      throw new Error('input must be an object');
    }
    return parsed;
  }

  async function runPreflight() {
    if (!selectedWorkflow) return false;
    setPreflightBusy(true);
    setStartError('');
    try {
      const report = await onPreflight(selectedWorkflow.package.id, buildStartInput());
      setPreflight(report);
      return report.ready;
    } catch (error) {
      setPreflight(null);
      if (error instanceof WorkflowStartFieldError) {
        revealStartFieldError(error.fieldId, error.message);
        return false;
      }
      setStartError(error instanceof Error ? error.message : '启动前检查失败');
      return false;
    } finally {
      setPreflightBusy(false);
    }
  }

  async function saveCredentialFile(connectorId: string, handle: string) {
    const saved = await onSaveCredentialFile(connectorId, handle);
    if (saved) await runPreflight();
    return saved;
  }

  async function saveCredentialSecret(connectorId: string, handle: string, secret: string) {
    await onSaveCredentialSecret(connectorId, handle, secret);
    await runPreflight();
  }

  async function submitStart() {
    if (!selectedWorkflow) return;
    setStartBusy(true);
    setStartError('');
    try {
      const input = buildStartInput();
      const report = preflight || await onPreflight(selectedWorkflow.package.id, input);
      setPreflight(report);
      if (!report.ready) {
        setStartError(report.blockers[0] || '启动前检查未通过');
        return;
      }
      // 启动后直接落到这次运行上：用户马上能在详情里看到当前步骤在跑，
      // 而不是回到列表里自己找刚才那一条。
      const started = await onStart(selectedWorkflow.package.id, input);
      setSelectedWorkflowId(selectedWorkflow.package.id);
      setView('runs');
      if (started?.run_id) {
        setStartedRunId(started.run_id);
        // 先给出受理回执并开始装详情，详情就绪后再关弹窗：
        // 用户看到的是「已受理 → 进入运行视图」，而不是点一下就没反应。
        setStartReceipt(started.run_id);
        await openRun(started.run_id);
      }
      setStartOpen(false);
      setStartReceipt('');
    } catch (submitError) {
      const message = submitError instanceof Error ? submitError.message : '';
      if (submitError instanceof WorkflowStartFieldError) {
        revealStartFieldError(submitError.fieldId, message);
      } else {
        setStartError(startMode === 'json'
          ? '输入必须是有效的 JSON 对象'
          : message || '结构化字段需要是有效内容；列表和 JSON 字段请检查格式');
      }
    } finally {
      setStartBusy(false);
    }
  }

  function setStartField(fieldId: string, value: unknown) {
    setPreflight(null);
    setStartForm(current => ({ ...current, [fieldId]: value }));
    setStartFieldErrors(current => {
      if (!current[fieldId]) return current;
      const next = { ...current };
      delete next[fieldId];
      return next;
    });
  }

  // 把错误提示放回出错字段：滚动到可见位置并聚焦输入框，
  // 否则用户只能看到底部一句提示，却不知道它指哪一行（长表单里尤其明显）。
  function revealStartFieldError(fieldId: string, message: string) {
    setStartError('');
    setStartFieldErrors(current => ({ ...current, [fieldId]: message }));
    window.requestAnimationFrame(() => {
      const container = startFormRef.current;
      if (!container) return;
      const target = container.querySelector<HTMLElement>(`[data-field-id="${fieldId}"]`);
      if (!target) return;
      target.scrollIntoView({ block: 'center' });
      target.querySelector<HTMLElement>('input:not([type="checkbox"]), textarea, select')?.focus({ preventScroll: true });
    });
  }

  async function pickDirectory(fieldId: string) {
    const path = await onPickDirectory();
    if (path) setStartField(fieldId, path);
  }

  async function installLocalArchive() {
    setLocalInstallBusy(true);
    try {
      await onInstallLocal();
    } finally {
      setLocalInstallBusy(false);
    }
  }

  async function loadPresets(workflowId: string) {
    if (!workflowId) {
      setPresets([]);
      return;
    }
    try {
      const result = await onLoadPresets(workflowId);
      setPresets(result.presets || []);
      setPresetError('');
    } catch {
      setPresets([]);
      setPresetError('启动预设读取失败');
    }
  }

  async function saveCurrentAsPreset() {
    if (!selectedWorkflow) return;
    let input: Record<string, unknown>;
    try {
      input = buildStartInput();
    } catch {
      setPresetError('当前参数还不完整，先把必填项填好再保存预设');
      return;
    }
    setPresetBusy('save');
    setPresetError('');
    try {
      await onSavePreset({
        workflow_id: selectedWorkflow.package.id,
        label: presetLabel.trim() || undefined,
        input,
      });
      setPresetLabel('');
      await loadPresets(selectedWorkflow.package.id);
    } catch {
      setPresetError('保存预设失败');
    } finally {
      setPresetBusy('');
    }
  }

  async function removePreset(id: string) {
    setPresetBusy(`delete:${id}`);
    setPresetError('');
    try {
      await onDeletePreset(id);
      if (selectedWorkflow) await loadPresets(selectedWorkflow.package.id);
    } catch {
      setPresetError('删除预设失败');
    } finally {
      setPresetBusy('');
    }
  }

  function workspaceHint(input: Record<string, unknown>) {
    const value = input?.workspace_root ?? input?.project_root ?? input?.source_root;
    return typeof value === 'string' && value.trim() ? value.trim() : '未指定工作区';
  }

  return (
    <div className="workflow-page">
      <PageHeader
        title="工作流"
        description="启动工作流，查看运行结果和输出文件。"
        actions={
          <>
            <button className="btn btn-icon" title="刷新工作流" aria-label="刷新工作流" onClick={onRefresh}><RefreshCw size={16} className={loading ? 'spin' : ''} /></button>
          </>
        }
      />
      {error ? <div className="blocker"><FileText size={18} /><div><strong>工作流数据读取失败</strong><span>{error}</span></div></div> : null}
      <section className="workflow-summary" aria-label="工作流概览">
        <div className={pendingCount ? 'attention' : ''}><Clock3 size={18} /><span><small>待处理</small><strong>{pendingCount}</strong></span></div>
        <div><Play size={18} /><span><small>运行中</small><strong>{runningCount}</strong></span></div>
        <div><CircleAlert size={18} /><span><small>失败</small><strong>{failedCount}</strong></span></div>
        <div><Workflow size={18} /><span><small>已安装工作流</small><strong>{workflows.length}</strong></span></div>
      </section>
      <div className="workflow-mode-tabs segmented-control" role="tablist" aria-label="工作流视图">
        <button type="button" role="tab" aria-selected={view === 'runs'} className={view === 'runs' ? 'active' : ''} onClick={() => setView('runs')}>运行任务 <span>{pendingCount + runningCount}</span></button>
        <button type="button" role="tab" aria-selected={view === 'library'} className={view === 'library' ? 'active' : ''} onClick={() => setView('library')}>工作流库 <span>{workflows.length}</span></button>
      </div>
      <div className="workflow-layout">
        <section className="card workflow-list-panel">
          {view === 'runs' ? <>
            <div className="card-header"><strong>待处理与最近运行</strong><Pill kind={pendingCount ? 'warn' : 'neutral'}>{filteredRuns.length}</Pill></div>
            <div className="workflow-run-filters">
              <select value={runWorkflowFilter} onChange={event => setRunWorkflowFilter(event.target.value)}>
                <option value="all">全部工作流</option>
                {workflows.map(item => <option key={item.package.id} value={item.package.id}>{item.package.name}</option>)}
              </select>
              <select value={runStatusFilter} onChange={event => setRunStatusFilter(event.target.value)}>
                <option value="all">全部状态</option>
                <option value="waiting">等待处理</option>
                <option value="running">运行中</option>
                <option value="failed">失败</option>
                <option value="succeeded">已完成</option>
              </select>
            </div>
            <div className="workflow-run-list enriched">
              {filteredRuns.map(item => (
                <button
                  type="button"
                  key={item.run.run_id}
                  className={[selectedRunId === item.run.run_id ? 'active' : '', ['queued', 'running', 'waiting'].includes(item.run.status) ? 'is-live' : ''].filter(Boolean).join(' ')}
                  onClick={() => void openRun(item.run.run_id)}
                >
                  <span>
                    <strong>{item.workflow_name || item.workflow_id || item.run.run_id}</strong>
                    <small>{item.business_stage || statusLabel(item.run.status)}{item.current_step_title ? ` · ${item.current_step_title}` : ''}</small>
                    {item.run.status === 'waiting' && item.waiting_reason ? <small className="workflow-run-waiting-reason">{item.waiting_reason}</small> : null}
                    {item.run.status === 'waiting' && item.required_action ? <small className="workflow-run-required-action">{interactionActionLabel(item.required_action)}</small> : null}
                    <small>
                      {item.app_id || item.project_root || '未记录项目'} · {['queued', 'running', 'waiting'].includes(item.run.status)
                        ? `更新于 ${formatIdleCn(Math.max(0, Math.floor(nowMs / 1000) - toEpochSeconds(item.run.updated_at)))}`
                        : formatTime(item.run.updated_at)}
                    </small>
                    {item.run.status === 'failed' && item.run.error ? <small className="workflow-run-error-hint">{item.run.error}</small> : null}
                  </span>
                  <Pill kind={statusKind(item.run.status)}>{item.run.status === 'waiting' ? waitingKindLabel(item.waiting_kind) : statusLabel(item.run.status)}</Pill>
                </button>
              ))}
          {!loading && filteredRuns.length === 0 ? <EmptyState icon={Clock3} title="暂无运行任务" text="从工作流库选择工作流并启动。" /> : null}
            </div>
          </> : <>
              <div className="card-header"><strong>已安装工作流</strong><span className="workflow-library-actions"><Pill kind="neutral">{workflows.length}</Pill><button type="button" className="btn btn-icon" title="安装本地工作流包" aria-label="安装本地工作流包" disabled={localInstallBusy} onClick={() => void installLocalArchive()}><Upload size={14} /></button></span></div>
            <div className="workflow-list">
              {workflows.map(item => (
                <button
                  type="button"
                  key={item.package.id}
                  className={selectedWorkflow?.package.id === item.package.id ? 'active' : ''}
                  onClick={() => setSelectedWorkflowId(item.package.id)}
                >
                  <span className="workflow-list-mark"><Workflow size={16} /></span>
                  <span>
                    <strong>{item.package.name}</strong>
                    <small>v{item.package.version} · {item.package.steps.length} 步</small>
                  </span>
                  <Pill kind={item.enabled ? 'success' : 'neutral'}>{item.enabled ? '已启用' : '已停用'}</Pill>
                </button>
              ))}
              {!loading && workflows.length === 0 ? (
                <div className="workflow-library-empty">
                  <EmptyState icon={Workflow} title="还没有安装工作流" text="到「市场」里浏览并安装工作流。" />
                  <button type="button" className="btn btn-primary" onClick={onOpenExtensions}><Store size={14} />浏览市场里的工作流</button>
                </div>
              ) : null}
            </div>
          </>}
        </section>
        <section className="card workflow-detail-panel">
          {view === 'runs' ? (
            runLoading ? <div className="page-loading"><span className="spinner" />正在读取运行详情</div>
              : runDetail ? (
              <>
                {runError ? <div className="workflow-inline-error">{runError}</div> : null}
                <div className={`workflow-run-hero ${timeline?.active ? 'is-live' : ''}`}>
                  <div>
                    <span>{runDetail.workflow?.package.id || runDetail.run.run_id}</span>
                    <h2>{runDetail.workflow?.package.name || '工作流运行'}</h2>
                    <p>{timeline?.headline || statusLabel(runDetail.run.status)}</p>
                  </div>
                  <div className="workflow-run-hero-side">
                    {timeline?.active ? <span className="workflow-run-pulse" aria-hidden="true" /> : null}
                    <Pill kind={statusKind(runDetail.run.status)}>{runDetail.run.status === 'waiting' ? waitingKindLabel(currentWaitingKind) : statusLabel(runDetail.run.status)}</Pill>
                  </div>
                </div>
                {timeline ? (
                  <div className={`workflow-run-live-panel${startedRunId === runDetail.run.run_id ? ' is-just-started' : ''}`}>
                    <div className="workflow-run-live-stats">
                      <div><span>已运行</span><strong>{formatElapsedCn(timeline.elapsedSeconds)}</strong></div>
                      <div><span>步骤进度</span><strong>{timeline.completed}/{timeline.total}</strong></div>
                      <div>
                        <span>最近更新</span>
                        <strong className={timeline.active && timeline.idleSeconds >= 45 ? 'is-idle' : ''}>
                          {timeline.active ? formatIdleCn(timeline.idleSeconds) : formatClock(toEpochSeconds(runDetail.run.updated_at))}
                        </strong>
                      </div>
                      <div><span>运行号</span><strong>{runDetail.run.run_id.slice(-8)}</strong></div>
                    </div>
                    <div className={`workflow-run-progress${timeline.active ? ' is-live' : ''}${!timeline.active && timeline.percent === 100 ? ' is-done' : ''}`} role="progressbar" aria-valuenow={timeline.percent} aria-valuemin={0} aria-valuemax={100} aria-label="步骤完成度">
                      <i style={{ width: `${timeline.percent}%` }} />
                    </div>
                    {timeline.active && timeline.idleSeconds >= 45 ? (
                      <p className="workflow-run-live-note idle" role="status">
                        <Clock3 size={13} />
                        最近一次事件在 {formatIdleCn(timeline.idleSeconds)}，运行仍在继续。
                      </p>
                    ) : null}
                  </div>
                ) : null}
                {waitingForFeedback ? (
                  <section className="workflow-action-card feedback">
                    <div><MessageCircle size={18} /><div><strong>{waitingRequest?.title || '需要你的反馈'}</strong><span>{currentFeedbackQuestion(runDetail) || '说明下一轮需要修改或验证的内容。'}</span><small>{waitingRequest?.required_action ? interactionActionLabel(waitingRequest.required_action) : '提交后继续运行'}</small>{waitingRequest?.schema ? <details className="workflow-interaction-schema"><summary>查看提交格式</summary><pre>{JSON.stringify(waitingRequest.schema, null, 2)}</pre></details> : null}</div></div>
                    <textarea
                      className="workflow-run-feedback"
                      value={resumeFeedback}
                      onChange={event => setResumeFeedback(event.target.value)}
                      placeholder="说明下一轮需要修改或验证的内容"
                      maxLength={8000}
                    />
                    <button type="button" className="btn btn-primary" disabled={Boolean(actionBusy) || !resumeFeedback.trim()} onClick={() => void performRunAction(runDetail.run.run_id, 'resume', async () => { await onResume(runDetail.run.run_id, resumeFeedback.trim()); setResumeFeedback(''); })}>
                      <CheckCircle2 size={14} />{actionBusy === 'resume' ? '提交中' : '提交反馈并继续'}
                    </button>
                  </section>
                ) : waitingForApproval ? (
                  <section className="workflow-action-card approval">
                    <div><ShieldCheck size={18} /><div><strong>{waitingRequest?.title || currentStepDefinition?.title || '等待审批'}</strong><span>{approvalDescription(currentStepDefinition, runDetail.run, waitingRequest)}</span>{waitingRequest?.risk_level ? <small>{riskLevelLabel(waitingRequest.risk_level)}风险</small> : null}{waitingRequest?.schema ? <details className="workflow-interaction-schema"><summary>查看提交格式</summary><pre>{JSON.stringify(waitingRequest.schema, null, 2)}</pre></details> : null}</div></div>
                    <div className="workflow-run-actions">
                      <button type="button" className="btn btn-primary" disabled={Boolean(actionBusy)} onClick={() => void performRunAction(runDetail.run.run_id, 'approve', () => onApprove(runDetail.run.run_id, runDetail.run.current_step_id))}><CheckCircle2 size={14} />{actionBusy === 'approve' ? '处理中' : '批准并继续'}</button>
                      <button type="button" className="btn" disabled={Boolean(actionBusy)} onClick={() => void performRunAction(runDetail.run.run_id, 'reject', () => onReject(runDetail.run.run_id, runDetail.run.current_step_id))}>拒绝</button>
                    </div>
                  </section>
                ) : waitingForExternal || waitingForOtherInteraction ? (
                  <section className="workflow-action-card external-wait">
                    <div><Clock3 size={18} /><div><strong>{waitingRequest?.title || '等待外部状态或人工操作'}</strong><span>{waitingRequest?.description || '这里暂时没有可执行操作，请查看任务动态和外部系统状态。'}</span><small>{waitingRequest?.required_action ? interactionActionLabel(waitingRequest.required_action) : '检查运行状态'}</small>{waitingRequest?.schema ? <details className="workflow-interaction-schema"><summary>查看提交格式</summary><pre>{JSON.stringify(waitingRequest.schema, null, 2)}</pre></details> : null}</div></div>
                    <div className="workflow-run-actions"><button type="button" className="btn" onClick={() => void openRun(runDetail.run.run_id)}><RefreshCw size={14} />刷新状态</button></div>
                  </section>
                ) : null}
                <div className="workflow-run-context">
                  <div><span>工作流</span><strong>{runDetail.workflow?.package.name || selectedWorkflow?.package.name || '未记录'}</strong></div>
                  <div><span>当前步骤</span><strong>{currentStepDefinition?.title || runDetail.run.current_step_id || '已完成'}</strong></div>
                  <div><span>运行环境</span><strong>{runtimeProviderLabel(runDetail.run.runtime_provider)}</strong></div>
                  <div>
                    <span>AI 模型</span>
                    <strong title={runtimeFacts?.endpoint || ''}>
                      {runtimeFacts
                        ? `${runtimeFacts.model}${runtimeFacts.service ? ` · ${serviceLabel(runtimeFacts.service)}` : ''}`
                        : hasRuntimeStep
                          ? 'AI 步骤未成功，无模型信息'
                          : '该运行未调用 AI'}
                    </strong>
                  </div>
                  <div><span>耗时</span><strong>{formatElapsedCn(timeline?.elapsedSeconds ?? runElapsedSeconds(runDetail.run))}</strong></div>
                </div>
                {runDetail.run.error ? <div className="workflow-inline-error">{runDetail.run.error}</div> : null}
                <section className="workflow-section">
                  <div className="workflow-section-title"><strong>执行步骤</strong><span>{timeline ? `${timeline.completed}/${timeline.total} 完成` : `${runDetail.run.steps.length} 步`}</span></div>
                  <div className="workflow-run-step-timeline">
                    {(timeline?.steps || []).map(step => (
                      <div key={step.id} className={`${step.state}${step.state === 'active' ? ' is-running' : ''}`}>
                        <i />
                        <span>
                          <strong>{step.title}</strong>
                          <small>
                            {[stepKindLabel(step.kind || step.executionMode), step.durationSeconds ? `耗时 ${formatElapsedCn(step.durationSeconds)}` : '', step.attempt > 1 ? `第 ${step.attempt} 次尝试` : ''].filter(Boolean).join(' · ')}
                          </small>
                          {step.error ? <small className="workflow-run-step-error">{step.error}</small> : null}
                          {step.artifacts.length ? (
                            <span className="workflow-run-step-artifacts">
                              {step.artifacts.map(artifact => <b key={artifact.id}>{artifact.name}</b>)}
                            </span>
                          ) : null}
                        </span>
                        <Pill kind={runStepStateKind(step.state)}>{step.running ? (step.state === 'waiting' ? '等待处理' : `进行中 ${formatElapsedCn(step.durationSeconds)}`) : runStepStateLabel(step.state)}</Pill>
                      </div>
                    ))}
                  </div>
                </section>
                <section className="workflow-section">
                  <div className="workflow-section-title"><strong>任务动态</strong><span>{activity.length ? `${activity.length} 条` : '等待事件'}</span></div>
                  <div className="workflow-run-activity">
                    {activity.map(item => (
                      <div key={item.key} className={item.kind}>
                        <time>{item.clock}</time>
                        <span>
                          <strong>{item.text}</strong>
                          {item.detail ? <small>{item.detail}</small> : null}
                        </span>
                      </div>
                    ))}
                    {!activity.length ? <div className="workflow-start-empty">还没有事件。</div> : null}
                  </div>
                </section>
                <section className="workflow-section">
                  <div className="workflow-section-title"><strong>输出文件</strong><span>{runDetail.run.artifacts.length}</span></div>
                  <div className="workflow-run-artifacts">
                    {runDetail.run.artifacts.map(artifact => (
                      <div key={artifact.artifact_id}>
                        <FileText size={14} />
                        <span><strong>{artifact.name}</strong><small>{formatBytes(artifact.size_bytes)}</small></span>
                        <button type="button" className="btn btn-icon" title="在文件夹中显示" aria-label={`在文件夹中显示 ${artifact.name}`} disabled={Boolean(actionBusy)} onClick={() => void onRevealArtifact(runDetail.run.run_id, artifact.artifact_id)}><FolderOpen size={14} /></button>
                      </div>
                    ))}
                    {!runDetail.run.artifacts.length ? <div className="workflow-start-empty">当前还没有输出文件。</div> : null}
                  </div>
                </section>
                {runDetail.run.status === 'succeeded' ? <section className="workflow-section"><button type="button" className="btn" disabled={Boolean(actionBusy)} onClick={() => void verifyRun(runDetail.run.run_id)}><ShieldCheck size={14} />{actionBusy === 'verify' ? '验证中' : '验证结果'}</button></section> : null}
                {runVerification ? (
                  <section className="workflow-verification">
                    <div><ShieldCheck size={16} /><span><strong>验证通过</strong><small>{runVerification.candidate_id.slice(0, 16)} · {runVerification.commit_sha.slice(0, 12)} · {runVerification.signature_key_id || '未签名'}</small></span></div>
                    {runVerification.artifacts.map(artifact => <div key={artifact.artifact_id}><span><strong>{artifact.artifact_id}</strong><small>{artifact.sha256.slice(0, 16)} · {formatBytes(artifact.size_bytes)}</small></span><Pill kind={artifact.candidate_bound ? 'success' : 'neutral'}>{artifact.candidate_bound ? '已验证' : '未绑定'}</Pill></div>)}
                  </section>
                ) : null}
                {runDetail.run.status === 'running' || runDetail.run.status === 'queued' || runDetail.run.status === 'waiting' ? <div className="workflow-run-actions"><button type="button" className="btn btn-danger-quiet" disabled={Boolean(actionBusy)} onClick={() => void performRunAction(runDetail.run.run_id, 'cancel', () => onCancel(runDetail.run.run_id))}>{actionBusy === 'cancel' ? '正在取消' : '取消运行'}</button></div> : null}
              </>
            ) : runError ? <div className="blocker"><CircleAlert size={18} /><div><strong>运行详情读取失败</strong><span>{runError}</span></div></div>
              : <EmptyState icon={Clock3} title="选择运行任务" text="左侧优先显示等待反馈、等待审批和运行中的任务。" />
          ) : selectedWorkflow ? (
            <>
              <div className="workflow-detail-head">
                <div>
                  <span>工作流</span>
                  <h2>{selectedWorkflow.package.name}</h2>
                  <p>{selectedWorkflow.package.description}</p>
                </div>
                <div className="workflow-detail-actions">
                  <Pill kind={selectedWorkflow.enabled ? 'success' : 'neutral'}>{selectedWorkflow.enabled ? '可运行' : '已停用'}</Pill>
                  <button
                    type="button"
                    className="btn"
                    title="到「定时任务」为这个工作流建立计划"
                    onClick={() => onScheduleWorkflow(selectedWorkflow.package.id)}
                  >
                    <CalendarClock size={14} />加定时计划
                  </button>
                  <button
                    type="button"
                    className="btn btn-icon"
                    title={selectedWorkflow.enabled ? '停用工作流' : '启用工作流'}
                    aria-label={selectedWorkflow.enabled ? '停用工作流' : '启用工作流'}
                    disabled={Boolean(packageBusy)}
                    onClick={() => void performPackageAction(
                      selectedWorkflow.enabled ? 'disable' : 'enable',
                      () => onSetEnabled(selectedWorkflow.package.id, !selectedWorkflow.enabled),
                    )}
                  >
                    <Power size={14} />
                  </button>
                  {selectedWorkflow.previous_version ? (
                    <button
                      type="button"
                      className="btn btn-icon"
                      title={`回滚到 v${selectedWorkflow.previous_version}`}
                      aria-label={`回滚到 v${selectedWorkflow.previous_version}`}
                      disabled={Boolean(packageBusy)}
                      onClick={() => void performPackageAction('rollback', () => onRollback(selectedWorkflow.package.id))}
                    >
                      <RotateCcw size={14} />
                    </button>
                  ) : null}
                  <button
                    type="button"
                    className="btn btn-icon btn-danger-quiet"
                    title="移除工作流"
                    aria-label="移除工作流"
                    disabled={Boolean(packageBusy)}
                    onClick={() => {
                      if (window.confirm(`确认移除工作流“${selectedWorkflow.package.name}”？`)) {
                        void performPackageAction('remove', () => onRemove(selectedWorkflow.package.id));
                      }
                    }}
                  >
                    <Trash2 size={14} />
                  </button>
                  <button type="button" className="btn btn-primary" disabled={!selectedWorkflow.enabled} onClick={() => openStart()}><Play size={14} />运行</button>
                </div>
              </div>
              <div className="workflow-meta-grid">
                <div><span>版本</span><strong>v{selectedWorkflow.package.version}</strong></div>
                <div><span>步骤</span><strong>{selectedWorkflow.package.steps.length}</strong></div>
                <div><span>支持环境</span><strong>{selectedWorkflow.package.supported_runtimes.length}</strong></div>
                <div><span>输出文件</span><strong>{selectedWorkflow.package.artifacts.length}</strong></div>
              </div>
              <div className="workflow-section">
                <div className="workflow-section-title">
                  <strong>运行指标</strong>
                  <span>共 {selectedWorkflow.metrics.terminal_runs} 次已结束运行</span>
                </div>
                <div className="workflow-meta-grid">
                  <div><span>完成率</span><strong>{Math.round(selectedWorkflow.metrics.completion_rate * 100)}%</strong></div>
                  <div><span>平均耗时</span><strong>{formatDuration(selectedWorkflow.metrics.average_duration_seconds)}</strong></div>
                  <div><span>返工次数</span><strong>{selectedWorkflow.metrics.rework_runs}</strong></div>
                  <div><span>人工介入</span><strong>{selectedWorkflow.metrics.approval_count + selectedWorkflow.metrics.feedback_wait_count}</strong></div>
                </div>
              </div>
              <div className="workflow-section">
                <div className="workflow-section-title">
                  <strong>启动预设</strong>
                  <span>{presets.length ? `${presets.length} 套参数` : '保存常用启动参数'}</span>
                </div>
                {presetError ? <div className="workflow-inline-error">{presetError}</div> : null}
                {presets.length ? (
                  <div className="workflow-preset-list">
                    {presets.map(preset => (
                      <div key={preset.id}>
                        <span>
                          <strong>{preset.label || preset.id}</strong>
                          <small>{workspaceHint(preset.input)}{preset.entrypoint ? ` · 入口 ${preset.entrypoint}` : ''}{preset.exitpoint ? ` · 出口 ${preset.exitpoint}` : ''}</small>
                        </span>
                        <button
                          type="button"
                          className="btn btn-primary"
                          disabled={!selectedWorkflow.enabled}
                          onClick={() => openStart(preset.input)}
                        >
                          <Play size={14} />使用
                        </button>
                        <button
                          type="button"
                          className="btn btn-icon btn-danger-quiet"
                          title="删除预设"
                          aria-label={`删除预设 ${preset.label || preset.id}`}
                          disabled={Boolean(presetBusy)}
                          onClick={() => void removePreset(preset.id)}
                        >
                          <Trash2 size={14} />
                        </button>
                      </div>
                    ))}
                  </div>
                ) : (
                  <div className="workflow-start-empty">
                    暂无预设。运行过程中可保存常用参数。
                  </div>
                )}
              </div>
              <div className="workflow-section workflow-graph-section">
                <div className="workflow-section-title">
                  <strong><GitBranch size={15} />执行结构</strong>
                  <span>{selectedWorkflowGraph?.nodes.length || 0} 个步骤 · {executionPolicyLabel(selectedWorkflow.package.execution_policy)}</span>
                </div>
                <div className="workflow-graph-summary">
                  <span><strong>{selectedWorkflowGraph?.entrypoints.length || 0}</strong> 个入口</span>
                  <span><strong>{selectedWorkflowGraph?.exits.length || 0}</strong> 个出口</span>
                  <span><strong>{selectedWorkflowGraph?.nodes.filter(node => node.loop).length || 0}</strong> 个循环</span>
                  <span><strong>{selectedWorkflow.package.artifacts.length}</strong> 类输出文件</span>
                </div>
                <div className="workflow-graph-list">
                  {(selectedWorkflowGraph?.nodes || []).map((node, index) => (
                    <div key={node.id} className="workflow-graph-node" style={{ marginLeft: `${Math.min(node.depth, 3) * 18}px` }}>
                      <span className="workflow-graph-node-index">{String(index + 1).padStart(2, '0')}</span>
                      <span className="workflow-graph-node-main">
                        <strong>{node.title}</strong>
                        <small>{stepKindLabel(node.kind)}{node.dependsOn.length ? ` · 前置：${node.dependsOn.map(id => id.split('/').pop()).join('、')}` : ' · 起始步骤'}{node.downstream.length ? ` · 后续 ${node.downstream.length}` : ''}{node.onFailure === 'continue' ? ' · 失败后继续' : ''}</small>
                        <span className="workflow-graph-tags">
                          {node.loop ? <b><Repeat2 size={11} />最多 {node.loop.maxIterations} 轮{node.loop.pauseForFeedback ? ' · 每轮可反馈' : ''}</b> : null}
                          {node.condition ? <b><GitBranch size={11} />条件：{node.condition}</b> : null}
                          {node.failureCondition ? <b><CircleAlert size={11} />失败条件：{node.failureCondition}</b> : null}
                          {node.inputArtifacts.length ? <b><Boxes size={11} />读取：{node.inputArtifacts.join('、')}</b> : null}
                          {node.producedArtifacts.length ? <b><ArrowDown size={11} />产出：{node.producedArtifacts.join('、')}</b> : null}
                        </span>
                      </span>
                      {node.approvalRequired ? <ShieldCheck size={14} aria-label="需要审批" /> : null}
                    </div>
                  ))}
                </div>
                {(selectedWorkflowGraph?.entrypoints.length || selectedWorkflowGraph?.exits.length) ? (
                  <div className="workflow-graph-endpoints">
                    {(selectedWorkflowGraph?.entrypoints || []).map(endpoint => <span key={`entry:${endpoint.id}`}><strong>入口</strong>{endpoint.label} · {endpoint.atStep}</span>)}
                    {(selectedWorkflowGraph?.exits || []).map(endpoint => <span key={`exit:${endpoint.id}`}><strong>出口</strong>{endpoint.label} · {endpoint.atStep}</span>)}
                  </div>
                ) : null}
              </div>
              <div className="workflow-section">
                <div className="workflow-section-title"><strong>步骤摘要</strong><span>{selectedWorkflow.package.steps.length} 步</span></div>
                <div className="workflow-step-list">
                  {selectedWorkflow.package.steps.map((step, index) => (
                    <div key={step.id}>
                      <span className="workflow-step-index">{String(index + 1).padStart(2, '0')}</span>
                    <span><strong>{step.title}</strong><small>{stepKindLabel(step.kind)}</small></span>
                      {step.approval_required ? <ShieldCheck size={15} aria-label="需要审批" /> : null}
                    </div>
                  ))}
                </div>
              </div>
              <div className="workflow-section">
                <div className="workflow-section-title"><strong>输出文件</strong><span>{selectedWorkflow.package.artifacts.length} 类</span></div>
                  <div className="workflow-artifact-list">
                    {selectedWorkflow.package.artifacts.map(artifact => (
                      <div key={artifact.id}><span><strong>{artifact.name}</strong><small>{artifact.validation ? '已启用格式校验' : '普通文件'}{artifact.max_bytes ? ` · 上限 ${formatBytes(artifact.max_bytes)}` : ''}</small></span><Pill kind={artifact.required ? 'warn' : 'neutral'}>{artifact.required ? '必需' : '可选'}</Pill></div>
                    ))}
                </div>
              </div>
            </>
          ) : <EmptyState icon={Workflow} title="请选择工作流" text="左侧列表用于查看步骤和输出文件。" />}
        </section>
      </div>
      {startOpen && selectedWorkflow ? (
        <div className="modal-backdrop" role="presentation" onClick={event => { if (event.currentTarget === event.target) setStartOpen(false); }}>
          <section className="modal workflow-start-modal" role="dialog" aria-modal="true" aria-labelledby="workflow-start-title">
            <div className="workflow-start-head">
              <div><span>工作流</span><h2 id="workflow-start-title">{selectedWorkflow.package.name}</h2></div>
              <button type="button" className="btn btn-icon" title="关闭" aria-label="关闭" onClick={() => setStartOpen(false)}><X size={16} /></button>
            </div>
            <div className="workflow-start-mode segmented-control" role="group" aria-label="输入模式">
              <button type="button" className={startMode === 'form' ? 'active' : ''} onClick={() => switchStartMode('form')}>表单</button>
              <button type="button" className={startMode === 'json' ? 'active' : ''} onClick={() => switchStartMode('json')}>JSON</button>
            </div>
            {startMode === 'form' ? (
              <div className="workflow-start-form" ref={startFormRef}>
                {selectedWorkflow.view?.sections.map(section => (
                  <section key={section.id}>
                    <h3>{section.title}</h3>
                    <div className="workflow-start-fields">
                      {(section.fields || []).map(rawField => {
                        const field = normalizeStartField(rawField);
                        const value = startForm[field.id];
                        const fieldError = startFieldErrors[field.id];
                        const label = <span>{field.label}{field.required ? <em>必需</em> : null}</span>;
                        const footer = (
                          <>
                            {fieldError ? <small className="workflow-start-field-error" role="alert">{fieldError}</small> : null}
                            {field.hint ? <small className="workflow-start-hint">{field.hint}</small> : null}
                          </>
                        );
                        const fieldClass = [usesFullRow(field) ? 'workflow-start-field-wide' : '', fieldError ? 'is-invalid' : ''].filter(Boolean).join(' ');
                        if (field.type === 'boolean') {
                          return (
                            <label key={field.id} data-field-id={field.id} className={['workflow-start-toggle', fieldError ? 'is-invalid' : ''].filter(Boolean).join(' ')}>
                              <input type="checkbox" checked={Boolean(value)} onChange={event => setStartField(field.id, event.target.checked)} />
                              <span>{field.label}</span>
                            </label>
                          );
                        }
                        if (field.type === 'select') {
                          return (
                            <label key={field.id} data-field-id={field.id} className={fieldClass}>
                              {label}
                              <select value={String(value || '')} aria-invalid={Boolean(fieldError)} onChange={event => setStartField(field.id, event.target.value)}>
                                <option value="">请选择</option>
                                {field.options.map(option => <option key={option} value={option}>{option}</option>)}
                              </select>
                              {footer}
                            </label>
                          );
                        }
                        if (field.type === 'textarea' || field.type === 'list' || field.type === 'json') {
                          return (
                            <label key={field.id} data-field-id={field.id} className={fieldClass}>
                              {label}
                              <textarea
                                rows={field.type === 'list' ? 3 : 4}
                                value={String(value ?? '')}
                                placeholder={field.placeholder || (field.type === 'list' ? '每行一项' : '')}
                                aria-invalid={Boolean(fieldError)}
                                onChange={event => setStartField(field.id, event.target.value)}
                              />
                              {footer}
                            </label>
                          );
                        }
                        // 目录字段由包的 picker 声明决定；保留字段名判断以兼容既有视图。
                        const directoryField = field.picker === 'directory'
                          || field.id === 'workspace_root'
                          || field.id === 'source_root'
                          || field.id === 'project_root';
                        return (
                          <label key={field.id} data-field-id={field.id} className={fieldClass}>
                            {label}
                            {directoryField ? (
                              <div className="workflow-path-picker">
                                <input
                                  type="text"
                                  value={String(value ?? '')}
                                  placeholder={field.placeholder}
                                  aria-invalid={Boolean(fieldError)}
                                  onChange={event => setStartField(field.id, event.target.value)}
                                />
                                <button type="button" className="btn btn-icon" title="选择目录" aria-label={`选择${field.label}`} onClick={() => void pickDirectory(field.id)}><FolderOpen size={14} /></button>
                              </div>
                            ) : (
                              <input
                                type={field.type === 'number' ? 'number' : 'text'}
                                value={String(value ?? '')}
                                placeholder={field.placeholder}
                                aria-invalid={Boolean(fieldError)}
                                onChange={event => setStartField(field.id, event.target.value)}
                              />
                            )}
                            {footer}
                          </label>
                        );
                      })}
                    </div>
                  </section>
                ))}
                {!(selectedWorkflow.view?.sections || []).some(section => (section.fields || []).length) ? (
                  <div className="workflow-start-empty">该工作流未声明表单输入，可切换到 JSON 模式。</div>
                ) : null}
              </div>
            ) : (
              <textarea className="workflow-start-json" value={startInput} onChange={event => { setStartInput(event.target.value); setPreflight(null); }} spellCheck={false} />
            )}
            {preflight ? <PreflightPanel report={preflight} busy={preflightBusy} uploadChannel={String(startForm.upload_channel || 'ci')} onSaveFile={saveCredentialFile} onSaveSecret={saveCredentialSecret} /> : null}
            {startReceipt ? (
              <div className="workflow-start-receipt" role="status">
                <CheckCircle2 size={18} />
                <div>
                  <strong>已受理</strong>
                  <span>运行记录即将打开</span>
                </div>
              </div>
            ) : null}
            <div className="workflow-start-footer">
              {startError ? <div className="workflow-inline-error" role="alert">{startError}</div> : null}
              {startBusy && !startReceipt ? (
                <div className="workflow-start-progress" role="status">
                  <LoaderCircle size={13} className="spin" />
                  正在校验并提交运行…
                </div>
              ) : null}
              <div className="workflow-start-preset-row">
                <input
                  type="text"
                  value={presetLabel}
                  placeholder="预设名称（例如：项目看板）"
                  aria-label="预设名称"
                  onChange={event => setPresetLabel(event.target.value)}
                />
                <button
                  type="button"
                  className="btn"
                  disabled={startBusy || presetBusy === 'save'}
                  onClick={() => void saveCurrentAsPreset()}
                >
                  {presetBusy === 'save' ? '保存中' : '保存为预设'}
                </button>
                <small>保存后可在工作流详情里一键启动，换工作区只改工作区字段。</small>
              </div>
              <div className="modal-actions">
                <button type="button" className="btn" onClick={() => setStartOpen(false)}>取消</button>
                <button type="button" className="btn" disabled={startBusy || preflightBusy} onClick={() => void runPreflight()}>{preflightBusy ? <><LoaderCircle className="spin" size={14} />检查中</> : '启动前检查'}</button>
                <button
                  type="button"
                  className="btn btn-primary"
                  title={preflight && !preflight.ready ? preflight.blockers[0] || '启动前检查未通过' : undefined}
                  disabled={startBusy || preflightBusy || Boolean(preflight && !preflight.ready)}
                  onClick={() => void submitStart()}
                >
                  <Play size={14} />{startBusy ? '正在启动' : '启动运行'}
                </button>
              </div>
            </div>
          </section>
        </div>
      ) : null}
    </div>
  );
}
