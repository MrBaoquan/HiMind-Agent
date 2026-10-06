import { useEffect, useMemo, useRef, useState } from 'react';
import type { KeyboardEvent as ReactKeyboardEvent } from 'react';
import { ArrowDown, ArrowLeft, Boxes, CalendarClock, CheckCircle2, ChevronRight, CircleAlert, Clock3, FileText, FolderOpen, GitBranch, KeyRound, MessageCircle, Pin, Play, Plus, RefreshCw, Repeat2, Search, ShieldCheck, Store, Trash2, Upload, Workflow, X } from 'lucide-react';
import { BusyIndicator } from '../components/BusyIndicator';
import { ExtensionKindMark } from '../components/ExtensionKindMark';
import { EmptyState, PageHeader, Pill } from '../components/Common';
import type { AcpRuntimeProfile, WorkflowCenterSnapshot, WorkflowLocalRun, WorkflowPreflight, WorkflowRunPreset, WorkflowRunPresetInput, WorkflowRunSnapshot, WorkflowRunVerification, WorkflowStep } from '../services/agentApi';
import { formatStamp } from '../timeFormat';
import {
  WorkflowStartFieldError,
  fieldsToInput,
  filledFieldCount,
  initialFormValues,
  initialFieldValue,
  invalidOptionValue,
  jsonToFormValues,
  normalizeStartField,
  usesFullRow,
  type WorkflowStartField,
} from './workflowStartForm';
import { blockerLead, blockerSummary, preflightNote } from './preflightNote';
import { tailPath } from './pathDisplay';
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
import { shortRunId } from './taskView';
import { runtimeProviderLabel } from './runtimeProviderView';

type WorkflowsPageProps = {
  snapshot: WorkflowCenterSnapshot | null;
  /**
   * ACP 执行方在「AI 连接 → 运行环境」里由用户命名，运行详情要还这个名字，
   * 否则 acp.* 一律落进「自定义运行环境」，看不出这一步是谁跑的。
   */
  runtimeProfiles?: AcpRuntimeProfile[];
  /**
   * 这个页面只服务两件事，两个实例分别承担：
   * - `runs`：工作流页自身，页签在「工作流（有哪些、怎么跑）」和「运行记录（跑成什么样）」之间切换；
   * - `library`：只渲染能力库（嵌在「我的能力 → 工作流」页签里），页签归容器。
   * 两处共用同一套实现，避免同一份工作流在两个入口里出现两种行为。
   */
  mode: 'runs' | 'library';
  /** 库模式的启动落点：启动后跳到运行记录页签看这次运行。 */
  onOpenRun?: (runId: string) => void;
  /** 兼容入口：早期版本用它跳「我的能力」，现在页签已经内建，仅保留传参不报错。 */
  onOpenCapabilities?: () => void;
  /** 从待处理中心或定时计划跳入时，直接打开指定 Run。 */
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
  onRemove: (packageId: string) => Promise<void>;
  onPickDirectory: () => Promise<string | null>;
  onOpenExtensions: () => void;
  /** 跳到平台级「定时计划」页，并预选该工作流作为定时目标。 */
  onScheduleWorkflow: (workflowId: string) => void;
  onLoadPresets: (workflowId: string) => Promise<{ presets: WorkflowRunPreset[] }>;
  onSavePreset: (input: WorkflowRunPresetInput) => Promise<void>;
  onDeletePreset: (id: string) => Promise<void>;
};

// 这一页对用户说的「方案」就是代码里的 preset：一套可复用的启动参数。
// 详情页只铺开常用的几套，再多就交给启动弹窗里的完整列表，否则一个工作流攒到
// 十几套方案时详情页会被一张表压满。
const PRESET_INLINE_LIMIT = 4;
// 方案多到需要找的时候才出现搜索框：两三条时多一个输入框只是噪声。
const PRESET_SEARCH_LIMIT = 6;

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
  const missingSkills = report.skills.filter(item => !item.available && item.required !== false).map(item => item.id);
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
       <span><strong>{report.ready ? '启动前检查通过' : '启动前检查未通过'}</strong><small>{report.ready ? '依赖和运行环境已就绪' : blockerSummary(report)}</small></span>
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
              {credentialBusy === `file:${credential.handle}` ? <BusyIndicator size={13} /> : <FolderOpen size={13} />}
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
                {credentialBusy === `secret:${credential.handle}` ? <BusyIndicator size={13} /> : <CheckCircle2 size={13} />}
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
    {blockerDiagnostics.length ? <details open><summary>{blockerDiagnostics.length} 个阻塞项</summary>{blockerDiagnostics.map(item => <p key={`${item.code}:${item.message}`}><strong>{blockerLead(item.code) || item.code}</strong> · {item.message}<br /><small>{item.remediation}{item.retryable ? ' 修复后可重试。' : ''}（{item.code}）</small></p>)}</details> : null}
    {warningDiagnostics.length ? <details><summary>{warningDiagnostics.length} 条非阻塞提示</summary>{warningDiagnostics.map(item => <p key={`${item.code}:${item.message}`}><strong>{item.code}</strong> · {item.message}<br /><small>{item.remediation}</small></p>)}</details> : null}
  </section>;
}

/**
 * 启动参数表单字段。启动弹窗和预设管理弹窗共用这一份渲染：
 * 同一个字段在两处长得一样，字段控件要改也只改这里。
 */
function StartFields({ sections, values, errors, onChange, onPickDirectory, containerRef }: {
  sections: Array<{ id: string; title: string; fields: WorkflowStartField[] }>;
  values: Record<string, unknown>;
  errors: Record<string, string>;
  onChange: (fieldId: string, value: unknown) => void;
  onPickDirectory: (fieldId: string) => void;
  containerRef?: { current: HTMLDivElement | null };
}) {
  return (
    <div className="workflow-start-form" ref={containerRef}>
      {sections.map(section => (
        <section key={section.id}>
          <h3>{section.title}</h3>
          <div className="workflow-start-fields">
            {section.fields.map(field => {
              const value = values[field.id];
              const fieldError = errors[field.id];
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
                    <input type="checkbox" checked={Boolean(value)} onChange={event => onChange(field.id, event.target.checked)} />
                    <span>{field.label}</span>
                  </label>
                );
              }
              if (field.type === 'select') {
                return (
                  <label key={field.id} data-field-id={field.id} className={fieldClass}>
                    {label}
                    <select value={String(value || '')} aria-invalid={Boolean(fieldError)} onChange={event => onChange(field.id, event.target.value)}>
                      <option value="">请选择</option>
                      {field.optionEntries.map(option => <option key={option.value} value={option.value}>{option.label}</option>)}
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
                      onChange={event => onChange(field.id, event.target.value)}
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
                        onChange={event => onChange(field.id, event.target.value)}
                      />
                      <button type="button" className="btn btn-icon" title="选择目录" aria-label={`选择${field.label}`} onClick={() => onPickDirectory(field.id)}><FolderOpen size={14} /></button>
                    </div>
                  ) : (
                    <input
                      type={field.type === 'number' ? 'number' : 'text'}
                      value={String(value ?? '')}
                      placeholder={field.placeholder}
                      aria-invalid={Boolean(fieldError)}
                      onChange={event => onChange(field.id, event.target.value)}
                    />
                  )}
                  {footer}
                </label>
              );
            })}
          </div>
        </section>
      ))}
      {!sections.some(section => section.fields.length) ? (
        <div className="workflow-start-empty">该工作流未声明表单输入，可切换到 JSON 模式。</div>
      ) : null}
    </div>
  );
}

/** 预设 id 只能含 ASCII 字母数字和 `.` `-` `_`，中文名称要通过这段转换才能落库。 */
function presetSlug(label: string) {
  return label.trim().toLocaleLowerCase().replace(/[^a-z0-9._-]+/g, '-').replace(/^[-._]+|[-._]+$/g, '').slice(0, 40);
}

/**
 * 预设 id 在整个预设文件里唯一（后端按 id upsert），所以新 id 必须带上工作流前缀：
 * 两个工作流各存一套同名预设时，否则后者会静默覆盖前者。
 */
function nextPresetId(label: string, taken: Set<string>, workflowId: string) {
  const scope = presetSlug(workflowId) || 'preset';
  const base = `${scope}-${presetSlug(label) || 'preset'}`.slice(0, 56);
  if (!taken.has(base)) return base;
  for (let index = 2; index < 500; index += 1) {
    const candidate = `${base}-${index}`;
    if (!taken.has(candidate)) return candidate;
  }
  return `${base}-${Date.now().toString(36)}`.slice(0, 64);
}

/** 工作区一律读预设里存下来的值：工作区本身就是一套参数的一部分。 */
function presetWorkspace(input: Record<string, unknown>) {
  const value = input?.workspace_root ?? input?.project_root ?? input?.source_root;
  return typeof value === 'string' ? value.trim() : '';
}

/**
 * 排序规则：钉过的「常用」永远在最前面，其余按最近改动排。
 * 常用在前是因为它们就是用户反复要用的那几套；最近改动在前是因为「运行」默认带出来的
 * 应该就是手上正在用的那套参数，而不是字母序里的第一条。
 * 时间戳是同一格式的秒级字符串，直接比较即可。
 */
function sortPresets(items: WorkflowRunPreset[]) {
  return [...items].sort((left, right) => {
    const pinnedDiff = Number(Boolean(right.pinned)) - Number(Boolean(left.pinned));
    if (pinnedDiff !== 0) return pinnedDiff;
    const stampDiff = presetRecency(right).localeCompare(presetRecency(left));
    if (stampDiff !== 0) return stampDiff;
    return (left.label || left.id).localeCompare(right.label || right.id);
  });
}

/** 同一格式的秒级字符串，直接比大小即可。 */
function presetRecency(preset: WorkflowRunPreset) {
  return preset.updated_at || preset.created_at || '';
}

function directoryLeaf(path: string) {
  const normalized = path.trim().replace(/\\/g, '/').replace(/\/+$/, '');
  return normalized.split('/').filter(Boolean).pop() || '';
}

/**
 * 预设管理的编辑草稿。`id` 为空表示新建：保存时才分配 id。
 * 「改名字」和「另存一份」因此是两件不同的事，不会因为输入框里的文字变了就悄悄换了一条预设。
 * `base` 保留视图没声明的键（例如自定义 report_root），编辑过程中不丢。
 */
type PresetDraft = {
  id: string;
  label: string;
  values: Record<string, unknown>;
  base: Record<string, unknown>;
  entrypoint: string;
  exitpoint: string;
  pinned: boolean;
};

function draftFromPreset(preset: WorkflowRunPreset | null, fields: WorkflowStartField[]): PresetDraft {
  const input = preset?.input || {};
  return {
    id: preset?.id || '',
    label: preset?.label || preset?.id || '',
    values: initialFormValues(fields, preset ? input : undefined),
    base: preset ? input : {},
    entrypoint: preset?.entrypoint || '',
    exitpoint: preset?.exitpoint || '',
    pinned: Boolean(preset?.pinned),
  };
}

export function WorkflowsPage({ snapshot, runtimeProfiles = [], mode, onOpenRun, initialRunId = '', loading, error, onRefresh, onLoadRun, onVerify, onRevealArtifact, onApprove, onReject, onResume, onCancel, onStart, onPreflight, onSaveCredentialFile, onSaveCredentialSecret, onInstallLocal, onSetEnabled, onRemove, onPickDirectory, onOpenExtensions, onScheduleWorkflow, onLoadPresets, onSavePreset, onDeletePreset }: WorkflowsPageProps) {
  // 工作流页把「有哪些能力」和「跑成什么样」拆成两个页签：进来先看到能跑的工作流，
  // 而不是一张运行记录表；有活动运行时才在列表上方补一条运行态。
  const [pageTab, setPageTab] = useState<'library' | 'runs'>(() => (mode === 'library' ? 'library' : initialRunId ? 'runs' : 'library'));
  const showLibrary = mode === 'library' || pageTab === 'library';
  const [selectedWorkflowId, setSelectedWorkflowId] = useState('');
  const [selectedRunId, setSelectedRunId] = useState('');
  const [runDetail, setRunDetail] = useState<WorkflowRunSnapshot | null>(null);
  const [runVerification, setRunVerification] = useState<WorkflowRunVerification | null>(null);
  const [runLoading, setRunLoading] = useState(false);
  const [runError, setRunError] = useState('');
  // 详情首次读取失败（例如刚启动的运行还没落盘）时的重试预算：一次瞬时抖动
  // 不该把面板永久钉在「运行详情读取失败」，也不该逼用户再点一次列表。
  const [runDetailRetry, setRunDetailRetry] = useState(0);
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
  // 破坏性操作（移除工作流、清掉读取失败的制品、删掉启动方案）统一走站内确认弹窗。
  // 原来这三处用 window.confirm：桌面端弹出来的是浏览器样式的系统框，和插件页
  // 卸载/停用那套 .modal 确认框不是一套东西——同一件「确认一下」在应用里长两个样子。
  const [pendingConfirm, setPendingConfirm] = useState<{ title: string; description: string; confirmText: string; run: () => void } | null>(null);
  const [startMode, setStartMode] = useState<'form' | 'json'>('form');
  const [startForm, setStartForm] = useState<Record<string, unknown>>({});
  const [preflight, setPreflight] = useState<WorkflowPreflight | null>(null);
  const [preflightBusy, setPreflightBusy] = useState(false);
  const [runWorkflowFilter, setRunWorkflowFilter] = useState('all');
  const [runStatusFilter, setRunStatusFilter] = useState('all');
  const [librarySearch, setLibrarySearch] = useState('');
  const [libraryStatusFilter, setLibraryStatusFilter] = useState<'all' | 'enabled' | 'disabled'>('all');
  const [presets, setPresets] = useState<WorkflowRunPreset[]>([]);
  const [presetBusy, setPresetBusy] = useState('');
  // 钉 / 取消钉是列表里的一次轻量写操作，单独占一个忙态：不该把整列方案的启动按钮一起冻住。
  const [pinBusy, setPinBusy] = useState('');
  const [presetLabel, setPresetLabel] = useState('');
  const [presetError, setPresetError] = useState('');
  // 启动弹窗固定在「先选参数、再启动」这条路径上：choose 选预设，params 看参数并启动。
  // 不再有「点运行直接套最近一套预设」的静默路径 —— 用户永远知道这一跑用的是哪套参数。
  const [startStep, setStartStep] = useState<'choose' | 'params'>('choose');
  // 参数态默认只露摘要，展开表单是显式动作。
  const [startEdit, setStartEdit] = useState(false);
  const [startPresetId, setStartPresetId] = useState('');
  // 保存动作是显式的：点「存为预设」才出现命名框，避免默认路径上多一个必填项。
  const [saveAsOpen, setSaveAsOpen] = useState(false);
  const [presetQuery, setPresetQuery] = useState('');
  const [presetsLoading, setPresetsLoading] = useState(false);
  // 预设管理弹窗：左列表 + 右表单，与「启动」分开，但复用同一套字段渲染。
  const [manageOpen, setManageOpen] = useState(false);
  const [manageDraft, setManageDraft] = useState<PresetDraft | null>(null);
  const [manageErrors, setManageErrors] = useState<Record<string, string>>({});
  const [manageMessage, setManageMessage] = useState('');
  const [manageQuery, setManageQuery] = useState('');
  const [manageBusy, setManageBusy] = useState('');
  // 预设里没被工作流视图声明的键（例如自定义 report_root）不能在编辑过程中丢掉。
  const [startBaseInput, setStartBaseInput] = useState<Record<string, unknown>>({});
  const workflows = snapshot?.workflows || [];
  // 读取失败但确实装在机器上的制品：不进可运行列表，但在同一个列表里现身，
  // 用户能看到「它坏了、坏在哪」，并且能一键清掉重装。
  const libraryIssues = useMemo(() => snapshot?.library_issues || [], [snapshot]);
  // 异常条目没有启用/停用状态，只跟着搜索词走：状态筛选是筛"能用的"，不该把它们藏起来。
  const visibleLibraryIssues = useMemo(() => {
    const query = librarySearch.trim().toLocaleLowerCase();
    if (!query) return libraryIssues;
    return libraryIssues.filter(issue => [issue.package_id, issue.version, issue.message].join(' ').toLocaleLowerCase().includes(query));
  }, [libraryIssues, librarySearch]);
  const runs = snapshot?.runs || [];
  const filteredWorkflows = useMemo(() => {
    const query = librarySearch.trim().toLocaleLowerCase();
    return workflows.filter(item => {
      if (libraryStatusFilter === 'enabled' && !item.enabled) return false;
      if (libraryStatusFilter === 'disabled' && item.enabled) return false;
      if (!query) return true;
      return [item.package.name, item.package.id, item.package.description, item.source].join(' ').toLocaleLowerCase().includes(query);
    });
  }, [librarySearch, libraryStatusFilter, workflows]);

  // 选中项跟着列表走：筛选或搜索把当前这条藏起来时，右侧不能还停在一条列表里找不到的
  // 工作流上。启停开关就在列表行尾，右侧显示一条列表里没有的条目，用户既看不到它现在
  // 是哪一档，也没有地方改；插件页早就是这个口径（搜索把选中项筛掉时详情跟着列表走）。
  const selectedWorkflow = useMemo(
    () => filteredWorkflows.find(item => item.package.id === selectedWorkflowId) || filteredWorkflows[0] || null,
    [selectedWorkflowId, filteredWorkflows],
  );
  const selectedWorkflowGraph = useMemo(() => (selectedWorkflow ? buildWorkflowGraph(selectedWorkflow.package) : null), [selectedWorkflow]);

  useEffect(() => {
    if (!selectedWorkflowId && filteredWorkflows[0]) setSelectedWorkflowId(filteredWorkflows[0].package.id);
  }, [selectedWorkflowId, filteredWorkflows]);

  useEffect(() => {
    if (!showLibrary) return;
    void loadPresets(selectedWorkflow?.package.id || '');
  }, [showLibrary, selectedWorkflow?.package.id]);

  async function openRun(runId: string) {
    setSelectedRunId(runId);
    setRunVerification(null);
    setResumeFeedback('');
    setRunLoading(true);
    setRunError('');
    setRunDetailRetry(0);
    try {
      setRunDetail(await onLoadRun(runId));
    } catch {
      setRunDetail(null);
      setRunError('运行详情读取失败');
    } finally {
      setRunLoading(false);
    }
  }

  /// 后台刷新不该挤掉已经渲染出来的详情；失败时只回报结果，由调用方决定要不要重试。
  async function reloadRunDetail(runId: string) {
    try {
      setRunDetail(await onLoadRun(runId));
      setRunError('');
      return true;
    } catch {
      return false;
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
    if (!initialRunId || mode !== 'runs') return;
    setPageTab('runs');
    void openRun(initialRunId);
  }, [initialRunId, mode]);

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
  // 已安装列表也要能看见「这个工作流正在跑」：按工作流归组活动运行，
  // 状态和计步都取自同一份运行中心快照，不额外发请求。
  const liveRunsByWorkflow = useMemo(() => {
    const map = new Map<string, { count: number; label: string; runId: string; startedAt: number | null }>();
    for (const item of runs) {
      if (!['queued', 'running', 'waiting'].includes(item.run.status)) continue;
      const current = map.get(item.workflow_id) || { count: 0, label: '', runId: '', startedAt: null };
      current.count += 1;
      if (!current.label) {
        current.label = item.run.status === 'waiting'
          ? (item.interaction_request?.kind === 'external_wait' ? '等待外部信号' : '等待处理')
          : item.current_step_title || item.business_stage || '';
        current.runId = item.run.run_id;
        // 以最早开始的那次为准：同一工作流的多个执行者叠加时，显示的是「等得最久的那条」。
        current.startedAt = toEpochSeconds(item.run.created_at);
      }
      map.set(item.workflow_id, current);
    }
    return map;
  }, [runs]);
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
  // 弹窗里选中的预设：决定「直接启动」带哪一套参数（含工作区）。
  const startPreset = presets.find(item => item.id === startPresetId) || null;
  const startFields = useMemo(
    () => (selectedWorkflow ? normalizedStartFields(selectedWorkflow) : []),
    [selectedWorkflow],
  );
  // 字段渲染用的分区结构：与 startFields 同源，只是保留分区标题。
  const startSections = useMemo(
    () => (selectedWorkflow?.view?.sections || []).map(section => ({
      id: section.id,
      title: section.title,
      fields: (section.fields || []).map(normalizeStartField),
    })),
    [selectedWorkflow],
  );
  // 预设多起来以后靠搜索收敛：名称、id、工作区路径都参与匹配。
  const visiblePresets = useMemo(() => {
    const query = presetQuery.trim().toLocaleLowerCase();
    if (!query) return presets;
    return presets.filter(preset => [preset.label, preset.id, presetWorkspace(preset.input)].join(' ').toLocaleLowerCase().includes(query));
  }, [presetQuery, presets]);
  // 「最近保存」徽标挂在真正最近改动的那一套上，而不是永远挂在第一行：
  // 钉过常用以后第一行是常用的那套，徽标挂错位置就变成误导。
  const recentPresetId = useMemo(() => {
    if (!presets.length) return '';
    return [...presets].sort((left, right) => presetRecency(right).localeCompare(presetRecency(left)))[0].id;
  }, [presets]);
  // 预设管理弹窗的左侧列表，与启动列表同一套匹配规则。
  const managePresets = useMemo(() => {
    const query = manageQuery.trim().toLocaleLowerCase();
    if (!query) return presets;
    return presets.filter(preset => [preset.label, preset.id, presetWorkspace(preset.input)].join(' ').toLocaleLowerCase().includes(query));
  }, [manageQuery, presets]);
  // 折叠后的摘要说的是「这一跑实际会用什么参数」，而不是打开弹窗时的那份快照：
  // 用户可能先改了两个字段再收起表单，工作区也得跟着走，否则摘要和真正的输入是两套值。
  // 计数口径与表单一致：布尔 false、空列表、空对象都不算填。
  const startSummary = useMemo(() => {
    let input: Record<string, unknown> = startBaseInput;
    try {
      const candidate = startMode === 'form'
        ? { ...startBaseInput, ...fieldsToInput(startFields, startForm) }
        : JSON.parse(startInput);
      if (candidate && typeof candidate === 'object' && !Array.isArray(candidate)) {
        input = candidate as Record<string, unknown>;
      }
    } catch {
      input = startBaseInput;
    }
    const values = startMode === 'form' ? startForm : jsonToFormValues(startFields, input);
    return {
      workspace: presetWorkspace(input),
      filled: filledFieldCount(startFields, values),
    };
  }, [startBaseInput, startFields, startForm, startInput, startMode]);

  useEffect(() => {
    if (!showLibrary && !selectedRunId && orderedRuns[0]) {
      void openRun(orderedRuns[0].run.run_id);
    }
  }, [showLibrary, orderedRuns, selectedRunId]);

  // 列表行是运行状态的权威来源：详情拿不到时也要能按这一行的状态继续轮询。
  // 只取状态字符串而不是整份 `runs`，避免父级每次刷新快照就重置轮询节奏。
  const selectedRunStatus = useMemo(
    () => runs.find(item => item.run.run_id === selectedRunId)?.run.status || '',
    [runs, selectedRunId],
  );

  useEffect(() => {
    const running = (status: string | undefined) => Boolean(status && ['queued', 'running', 'waiting'].includes(status));
    const detailActive = running(runDetail?.run.status);
    const listActive = running(selectedRunStatus);
    const retryingInitialLoad = !runDetail && !runLoading && runDetailRetry < 3;
    if (!selectedRunId || (!detailActive && !listActive && !retryingInitialLoad)) return;
    let disposed = false;
    const refreshDetail = async () => {
      if (disposed || document.visibilityState === 'hidden') return;
      // 详情拿不到时保留上一份快照：列表级轮询仍是权威，闪屏比重试更糟。
      const ok = await reloadRunDetail(selectedRunId);
      if (!disposed && !ok && !runDetail) setRunDetailRetry(current => current + 1);
    };
    const timer = window.setInterval(() => void refreshDetail(), 5000);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, [onLoadRun, runDetail, runDetailRetry, runLoading, selectedRunStatus, selectedRunId]);

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

  /**
   * 把一套参数装进启动表单。预设只覆盖它带有的字段，其余仍走包声明的默认值；
   * 表单值必须经过同一条 JSON→表单转换，否则数组会被拍成一行 "cv,llm,ar-vr"。
   */
  function loadStartInput(prefill?: Record<string, unknown>) {
    if (!selectedWorkflow) return;
    const fields = normalizedStartFields(selectedWorkflow);
    const values = initialFormValues(fields, prefill);
    setStartForm(values);
    setStartBaseInput(prefill || {});
    setStartInput(JSON.stringify({ ...(prefill || {}), ...fieldsToInput(fields, values) }, null, 2));
    setStartMode('form');
  }

  /**
   * 打开启动弹窗。所有「运行」入口都走这里，且一律先问「用哪套参数」：
   * 静默套用最近一套预设，用户没法确认这一跑会打到哪里。
   * 一套预设都没有时这一屏也是决策点：手动填参数，或先存一套再跑。
   * 从详情页某一套预设点进来时带上 preset：已经选过了，直接停在这套参数的确认页。
   */
  function openStart(preset?: WorkflowRunPreset) {
    if (!selectedWorkflow) return;
    setStartStep('choose');
    setStartEdit(false);
    setStartPresetId(preset?.id || '');
    setPresetQuery('');
    setPresetLabel('');
    setPresetError('');
    setSaveAsOpen(false);
    setStartError('');
    setStartFieldErrors({});
    setPreflight(null);
    loadStartInput(preset?.input);
    setStartOpen(true);
    // 从运行记录页签点进来时预设可能还没读过：打开即刷新，列表就是最新的。
    void loadPresets(selectedWorkflow.package.id);
    if (preset) chooseStartPreset(preset);
  }

  /** 选中一套预设 → 进参数步骤。缺必填项时直接展开表单，让用户在这里补齐。 */
  function chooseStartPreset(preset: WorkflowRunPreset) {
    if (!selectedWorkflow) return;
    const fields = normalizedStartFields(selectedWorkflow);
    const values = initialFormValues(fields, preset.input);
    setStartPresetId(preset.id);
    setPresetLabel('');
    setSaveAsOpen(false);
    setPresetError('');
    loadStartInput(preset.input);
    const missing = fields.find(field => field.required && !String(values[field.id] ?? '').trim());
    if (missing) {
      setStartEdit(true);
      setStartFieldErrors({ [missing.id]: `${missing.label}不能为空` });
      setStartError(`${preset.label || preset.id} 缺少必填项，补全后即可启动。`);
    } else {
      setStartEdit(false);
      setStartFieldErrors({});
      setStartError('');
    }
    setStartStep('params');
  }

  /** 不用任何预设：按包声明的默认参数起一张空表单。 */
  function startFromScratch() {
    if (!selectedWorkflow) return;
    setStartPresetId('');
    setStartEdit(true);
    setPresetLabel('');
    setSaveAsOpen(false);
    setPresetError('');
    setStartError('');
    setStartFieldErrors({});
    setPreflight(null);
    loadStartInput(undefined);
    setStartStep('params');
  }

  // 预设里的工作区就是这个预设要跑的地方：启动时按原样带过去，不再要求每次指定。
  function presetExecution(preset: WorkflowRunPreset | undefined) {
    const entrypoint = preset?.entrypoint?.trim() || '';
    const exitpoint = preset?.exitpoint?.trim() || '';
    return entrypoint || exitpoint ? { entrypoint, exitpoint } : null;
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
          `${invalid.field.label}不支持「${invalid.invalidValue}」，可选：${invalid.field.optionEntries.map(option => option.label).join('、')}`,
        );
      }
      // 预设里可能有视图没声明的键（例如自定义产物目录），它们要跟着这一跑继续传递。
      return { ...startBaseInput, ...fieldsToInput(fields, startForm) };
    }
    const parsed = JSON.parse(startInput);
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
      throw new Error('input must be an object');
    }
    return parsed;
  }

  // 真正提交给运行中心的输入：参数 + 预设声明的入口/出口。
  function effectiveStartInput(): Record<string, unknown> {
    const input = buildStartInput();
    const execution = presetExecution(presets.find(item => item.id === startPresetId));
    return execution ? { ...input, execution } : input;
  }

  async function runPreflight() {
    if (!selectedWorkflow) return false;
    setPreflightBusy(true);
    setStartError('');
    try {
      const report = await onPreflight(selectedWorkflow.package.id, effectiveStartInput());
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
      const input = effectiveStartInput();
      const report = preflight || await onPreflight(selectedWorkflow.package.id, input);
      setPreflight(report);
      if (!report.ready) {
        setStartError(preflightNote(report, '启动前检查未通过'));
        return;
      }
      // 启动后直接落到这次运行上：用户马上能在详情里看到当前步骤在跑，
      // 而不是回到列表里自己找刚才那一条。
      const started = await onStart(selectedWorkflow.package.id, input);
      setSelectedWorkflowId(selectedWorkflow.package.id);
      if (started?.run_id) {
        setStartedRunId(started.run_id);
        // 先给出受理回执并开始装详情，详情就绪后再关弹窗：
        // 用户看到的是「已受理 → 进入运行视图」，而不是点一下就没反应。
        setStartReceipt(started.run_id);
        // 工作流页自己就有运行详情，切到运行记录页签直接打开；
        // 能力库没有运行详情面板，于是把这次运行交给工作流页去展示。
        if (mode === 'runs') {
          setPageTab('runs');
          await openRun(started.run_id);
        } else {
          onOpenRun?.(started.run_id);
        }
      }
      setStartOpen(false);
      setStartReceipt('');
      setStartEdit(false);
      setStartPresetId('');
      setSaveAsOpen(false);
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
    setPresetsLoading(true);
    try {
      const result = await onLoadPresets(workflowId);
      setPresets(sortPresets(result.presets || []));
      setPresetError('');
    } catch {
      setPresets([]);
      setPresetError('启动方案读取失败');
    } finally {
      setPresetsLoading(false);
    }
  }

  /** 存成一套新方案。名称留空按工作区目录名兜底，但仍然落在一个新 id 上，不会覆盖别人。 */
  async function saveStartAsPreset() {
    if (!selectedWorkflow) return;
    const typed = presetLabel.trim();
    let input: Record<string, unknown>;
    try {
      input = buildStartInput();
    } catch (error) {
      if (error instanceof WorkflowStartFieldError) revealStartFieldError(error.fieldId, error.message);
      setPresetError('参数还不完整，先把必填项填好再保存方案');
      return;
    }
    const label = typed || directoryLeaf(presetWorkspace(input)) || `方案 ${presets.length + 1}`;
    const id = nextPresetId(label, new Set(presets.map(item => item.id)), selectedWorkflow.package.id);
    setPresetBusy('save');
    setPresetError('');
    try {
      await onSavePreset({
        id,
        workflow_id: selectedWorkflow.package.id,
        label,
        input,
        // 入口/出口属于预设的一部分：从某套预设派生出来时沿用，否则跟随工作流默认。
        entrypoint: startPreset?.entrypoint || '',
        exitpoint: startPreset?.exitpoint || '',
        // 新存的一份默认不钉：常用与否是用户后来自己决定的。
        pinned: false,
      });
      await loadPresets(selectedWorkflow.package.id);
      setStartPresetId(id);
      setPresetLabel('');
      setSaveAsOpen(false);
      // 保存完切回摘要态：预设已经是一套完整参数，直接启动就行，不用再对着一张表单。
      setStartEdit(false);
    } catch {
      setPresetError('保存方案失败');
    } finally {
      setPresetBusy('');
    }
  }

  /** 用当前参数覆盖正在使用的那套预设：名称、入口/出口按原样保留，只更新参数。 */
  async function updateCurrentPreset() {
    if (!selectedWorkflow || !startPreset) return;
    let input: Record<string, unknown>;
    try {
      input = buildStartInput();
    } catch (error) {
      if (error instanceof WorkflowStartFieldError) revealStartFieldError(error.fieldId, error.message);
      setPresetError('参数还不完整，先把必填项填好再更新方案');
      return;
    }
    setPresetBusy('update');
    setPresetError('');
    try {
      await onSavePreset({
        id: startPreset.id,
        workflow_id: selectedWorkflow.package.id,
        label: startPreset.label || startPreset.id,
        input,
        entrypoint: startPreset.entrypoint || '',
        exitpoint: startPreset.exitpoint || '',
        // 改参数不动置顶状态：后端只在显式传 pinned 时才改它，这里如实带上当前值。
        pinned: Boolean(startPreset.pinned),
      });
      await loadPresets(selectedWorkflow.package.id);
      setStartEdit(false);
    } catch {
      setPresetError('更新方案失败');
    } finally {
      setPresetBusy('');
    }
  }

  /** 打开预设管理：没指定就落在第一套预设上；一套都没有时给一张新建草稿。 */
  function openManage(presetId?: string) {
    if (!selectedWorkflow) return;
    const fields = normalizedStartFields(selectedWorkflow);
    const target = presets.find(item => item.id === presetId) || presets[0] || null;
    setManageDraft(draftFromPreset(target, fields));
    setManageErrors({});
    setManageMessage('');
    setManageQuery('');
    setManageOpen(true);
    // 列表可能已经落后（别处改过预设）：打开即刷新。
    void loadPresets(selectedWorkflow.package.id);
  }

  /** ↑↓ 在预设行之间移动焦点：键盘用户不必来回摸鼠标。 */
  function handlePickerKeyDown(event: ReactKeyboardEvent<HTMLDivElement>) {
    if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
    const rows = Array.from(event.currentTarget.querySelectorAll<HTMLElement>('[data-preset-row]'));
    if (!rows.length) return;
    const index = rows.indexOf(document.activeElement as HTMLElement);
    const next = event.key === 'ArrowDown'
      ? (index < 0 ? 0 : Math.min(index + 1, rows.length - 1))
      : (index < 0 ? rows.length - 1 : Math.max(index - 1, 0));
    event.preventDefault();
    rows[next]?.focus();
  }

  function setManageField(fieldId: string, value: unknown) {
    setManageDraft(current => (current ? { ...current, values: { ...current.values, [fieldId]: value } } : current));
    setManageErrors(current => {
      if (!current[fieldId]) return current;
      const next = { ...current };
      delete next[fieldId];
      return next;
    });
  }

  function newManageDraft() {
    if (!selectedWorkflow) return;
    setManageDraft(draftFromPreset(null, normalizedStartFields(selectedWorkflow)));
    setManageErrors({});
    setManageMessage('新方案：填好参数再保存。');
  }

  /** 复制成一张新草稿：新 id、新名字，确认保存后才会真正新增，原方案不动。 */
  function duplicateManageDraft() {
    if (!manageDraft) return;
    // 副本默认不钉：常用是「这一套」的属性，不是这一类方案的属性。
    setManageDraft(current => current ? { ...current, id: '', label: `${current.label || '方案'} 副本`, pinned: false } : current);
    setManageErrors({});
    setManageMessage('已复制成新草稿，保存后新增一套方案。');
  }

  async function saveManageDraft() {
    if (!selectedWorkflow || !manageDraft) return;
    const fields = normalizedStartFields(selectedWorkflow);
    const label = manageDraft.label.trim();
    if (!label) {
      setManageMessage('方案名称不能为空');
      return;
    }
    const errors: Record<string, string> = {};
    for (const field of fields) {
      if (field.required && !String(manageDraft.values[field.id] ?? '').trim()) errors[field.id] = `${field.label}不能为空`;
    }
    if (Object.keys(errors).length) {
      setManageErrors(errors);
      setManageMessage('还有必填项没填，补齐后即可保存。');
      return;
    }
    const input = { ...manageDraft.base, ...fieldsToInput(fields, manageDraft.values) };
    const id = manageDraft.id || nextPresetId(label, new Set(presets.map(item => item.id)), selectedWorkflow.package.id);
    setManageBusy('save');
    setManageMessage('');
    try {
      await onSavePreset({
        id,
        workflow_id: selectedWorkflow.package.id,
        label,
        input,
        entrypoint: manageDraft.entrypoint,
        exitpoint: manageDraft.exitpoint,
        pinned: manageDraft.pinned,
      });
      const result = await onLoadPresets(selectedWorkflow.package.id);
      const next = sortPresets(result.presets || []);
      setPresets(next);
      setPresetError('');
      setManageDraft(draftFromPreset(next.find(item => item.id === id) || null, fields));
      setManageMessage('已保存');
      // 下次打开启动弹窗默认选中的就是刚改过的这套参数。
      setStartPresetId(current => current || id);
    } catch {
      setManageMessage('保存失败，请稍后重试');
    } finally {
      setManageBusy('');
    }
  }

  async function deleteManageDraft() {
    if (!selectedWorkflow || !manageDraft?.id) return;
    const removedId = manageDraft.id;
    setManageBusy('delete');
    setManageMessage('');
    try {
      await onDeletePreset(removedId);
      const result = await onLoadPresets(selectedWorkflow.package.id);
      const next = sortPresets(result.presets || []);
      setPresets(next);
      setManageDraft(draftFromPreset(next[0] || null, normalizedStartFields(selectedWorkflow)));
      if (startPresetId === removedId) {
        setStartPresetId('');
      }
      setManageMessage('已删除');
    } catch {
      setManageMessage('删除失败，请稍后重试');
    } finally {
      setManageBusy('');
    }
  }

  /**
   * 一套预设要一眼看明白的三件事：打到哪个工作区、参数填了多少、走哪个入口出口。
   * 做成结构化字段而不是拼一串文字，列表里就能用标签排版，窄宽度下也不会互相挤。
   */
  function presetTags(preset: WorkflowRunPreset) {
    const fields = selectedWorkflow ? normalizedStartFields(selectedWorkflow) : [];
    const workspace = presetWorkspace(preset.input);
    const filled = filledFieldCount(fields, jsonToFormValues(fields, preset.input || {}));
    // 只有工作流真的声明了目录字段，「未指定工作区」才是一条有信息量的提示；
    // 其余场景每行都挂一个空格子只是噪声，省掉。
    const needsWorkspace = fields.some(field => field.id === 'workspace_root' || field.id === 'project_root' || field.id === 'source_root');
    return {
      workspace,
      workspaceLabel: workspace ? (tailPath(workspace) || workspace) : (needsWorkspace ? '未指定工作区' : ''),
      // 和「入口 x」「出口 y」同构：标签在前、取值在后，一行标签读下来语义一致。
      params: fields.length ? `参数 ${filled}/${fields.length}` : '',
      entrypoint: preset.entrypoint,
      exitpoint: preset.exitpoint,
    };
  }

  /** `full` 用于 title：鼠标悬停时给回完整工作区路径，列表行本身只放末两级。 */
  function presetSummary(preset: WorkflowRunPreset, options?: { full?: boolean }) {
    const tags = presetTags(preset);
    return [
      options?.full ? tags.workspace || tags.workspaceLabel : tags.workspaceLabel,
      tags.params,
      tags.entrypoint ? `入口 ${tags.entrypoint}` : '',
      tags.exitpoint ? `出口 ${tags.exitpoint}` : '',
    ].filter(Boolean).join(' · ');
  }

  /**
   * 预设行内直接运行：预设就是一套完整参数（含工作区），
   * 所以这里做的是「检查 → 启动」，不再把人拉回一张表单前面。
   */
  async function runPreset(preset: WorkflowRunPreset) {
    if (!selectedWorkflow || !selectedWorkflow.enabled) return;
    const fields = normalizedStartFields(selectedWorkflow);
    const values = initialFormValues(fields, preset.input);
    const missing = fields.find(field => field.required && !String(values[field.id] ?? '').trim());
    if (missing) {
      setStartOpen(true);
      chooseStartPreset(preset);
      return;
    }
    const input = { ...(preset.input || {}), ...fieldsToInput(fields, values) };
    const execution = presetExecution(preset);
    const payload = execution ? { ...input, execution } : input;
    setPresetBusy(`run:${preset.id}`);
    setPresetError('');
    try {
      const report = await onPreflight(selectedWorkflow.package.id, payload);
      if (!report.ready) {
        setPresetError(preflightNote(report, '启动前检查未通过，请先处理依赖再运行。'));
        return;
      }
      const started = await onStart(selectedWorkflow.package.id, payload);
      if (!started?.run_id) return;
      setStartedRunId(started.run_id);
      setStartOpen(false);
      setManageOpen(false);
      if (mode === 'runs') {
        setPageTab('runs');
        await openRun(started.run_id);
      } else {
        onOpenRun?.(started.run_id);
      }
    } catch (error) {
      setPresetError(error instanceof Error && error.message ? error.message : '启动失败，请查看运行记录');
    } finally {
      setPresetBusy('');
    }
  }

  /**
   * 钉 / 取消钉一套方案。
   *
   * 「常用」是这一套方案自己的属性，翻它不该顺带改参数、更不该换一个 id，所以回传的是
   * 这一套方案的原样快照，只让 pinned 这一位变；后端也只在这一个字段上做覆盖。
   */
  async function togglePresetPin(preset: WorkflowRunPreset) {
    if (!selectedWorkflow || pinBusy) return;
    const next = !preset.pinned;
    setPinBusy(preset.id);
    setPresetError('');
    try {
      await onSavePreset({
        id: preset.id,
        workflow_id: preset.workflow_id || selectedWorkflow.package.id,
        label: preset.label || preset.id,
        input: preset.input || {},
        entrypoint: preset.entrypoint || '',
        exitpoint: preset.exitpoint || '',
        pinned: next,
      });
      const result = await onLoadPresets(selectedWorkflow.package.id);
      setPresets(sortPresets(result.presets || []));
      // 正在编辑的就是这一套时，草稿里的钉位跟着走，否则再点保存会把它翻回去。
      setManageDraft(current => (current && current.id === preset.id ? { ...current, pinned: next } : current));
      setManageMessage(next ? '已置顶，排在列表最前面' : '已取消置顶');
    } catch {
      setPresetError(next ? '置顶失败' : '取消置顶失败');
    } finally {
      setPinBusy('');
    }
  }

  return (
    <div className="workflow-page">
      {/* 库模式渲染在「我的能力 → 工作流」页签里，页面标题与页签归容器。 */}
      {/* 页头不放「运行工作流」：发起运行属于「工作流」页签——那里右侧详情就有「运行」，
          挂在运行记录页头的那一枚按下去要先切回另一个页签才出现结果，动作与结果不在同一屏；
          而且它启动的是当前选中的工作流，与正在看的这条运行记录无关。
          运行记录是只读历史；没有记录时的空态已经给出「去看工作流」这一步。 */}
      {mode === 'runs' ? <>
      <PageHeader title="工作流" />
      <div className="workflow-mode-tabs segmented-control" role="tablist" aria-label="工作流视图">
        <button type="button" role="tab" aria-selected={pageTab === 'library'} className={pageTab === 'library' ? 'active' : ''} onClick={() => setPageTab('library')}>工作流<span>{workflows.length}</span></button>
        <button type="button" role="tab" aria-selected={pageTab === 'runs'} className={pageTab === 'runs' ? 'active' : ''} onClick={() => setPageTab('runs')}>运行记录<span>{runs.length}</span></button>
      </div>
      {error ? <div className="blocker"><FileText size={18} /><div><strong>工作流数据读取失败</strong><span>{error}</span></div></div> : null}
      {/* 只在真的有运行在进行/等待时出现：常态下这一条不该占位置。 */}
      {pendingCount + runningCount > 0 ? (
        <button type="button" className="workflow-live-bar" onClick={() => setPageTab('runs')}>
          <Clock3 size={14} />
          <strong>{[pendingCount ? `${pendingCount} 个等待处理` : '', runningCount ? `${runningCount} 个运行中` : ''].filter(Boolean).join(' · ')}</strong>
          <small>查看运行记录</small>
        </button>
      ) : null}
      </> : null}
      {mode === 'library' && error ? <div className="blocker"><FileText size={18} /><div><strong>工作流数据读取失败</strong><span>{error}</span></div></div> : null}
      <div className="workflow-layout">
        <section className="card workflow-list-panel">
          {!showLibrary ? <>
            <div className="card-header"><strong>运行记录</strong><Pill kind={pendingCount ? 'warn' : 'neutral'}>{filteredRuns.length}</Pill></div>
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
                        : formatStamp(item.run.updated_at)}
                    </small>
                    {item.run.status === 'failed' && item.run.error ? <small className="workflow-run-error-hint">{item.run.error}</small> : null}
                  </span>
                  <Pill kind={statusKind(item.run.status)}>{item.run.status === 'waiting' ? waitingKindLabel(item.waiting_kind) : statusLabel(item.run.status)}</Pill>
                </button>
              ))}
           {!loading && filteredRuns.length === 0 ? (
             <div className="workflow-library-empty workflow-run-empty">
               <EmptyState icon={Clock3} title={runs.length ? '没有匹配的运行任务' : '还没有运行记录'} text={runs.length ? '换一个工作流或状态筛选试试。' : '在「工作流」里选一个方案直接运行。'} />
             {!runs.length ? <button type="button" className="btn btn-primary" onClick={() => setPageTab('library')}><Workflow size={14} />去看工作流</button> : null}
             </div>
           ) : null}
            </div>
          </> : <>
              <div className="card-header"><strong>已安装工作流</strong><span className="workflow-library-actions">{libraryIssues.length ? <Pill kind="warn">{libraryIssues.length} 个读取失败</Pill> : null}<Pill kind="neutral">{filteredWorkflows.length}{filteredWorkflows.length !== workflows.length ? ` / ${workflows.length}` : ''}</Pill><button type="button" className="btn btn-icon" title="安装本地工作流包" aria-label="安装本地工作流包" disabled={localInstallBusy} onClick={() => void installLocalArchive()}><Upload size={14} /></button></span></div>
              <div className="workflow-library-filters" role="search">
                <label className="workflow-search">
                  <Search size={14} aria-hidden="true" />
                  <input value={librarySearch} placeholder="搜索工作流" aria-label="搜索工作流" onChange={event => setLibrarySearch(event.target.value)} />
                </label>
                {/* 状态词只留一套：列表、筛选、详情都说「已启用 / 已停用」。
                    详情胶囊原来写「可运行」，和列表里的「已启用」指的是同一件事。 */}
                <select value={libraryStatusFilter} aria-label="筛选工作流启用状态" onChange={event => setLibraryStatusFilter(event.target.value as typeof libraryStatusFilter)}>
                  <option value="all">全部状态</option>
                  <option value="enabled">已启用</option>
                  <option value="disabled">已停用</option>
                </select>
              </div>
            <div className="workflow-list">
              {visibleLibraryIssues.map(issue => (
                <div className="workflow-list-issue" key={`issue:${issue.package_id}`}>
                  <span className="workflow-list-mark is-danger"><CircleAlert size={16} /></span>
                  <span>
                    <strong>{issue.package_id}</strong>
                    <small>读取失败{issue.version ? ` · v${issue.version}` : ''} · 当前不可运行</small>
                    <small className="workflow-list-issue-reason" title={issue.message}>{issue.message}</small>
                  </span>
                  <button
                    type="button"
                    className="btn btn-icon btn-danger-quiet"
                    title={`移除 ${issue.package_id}`}
                    aria-label={`移除 ${issue.package_id}`}
                    disabled={Boolean(packageBusy)}
                    onClick={() => {
                      setPendingConfirm({
                        title: '确认移除这个制品？',
                        description: `移除“${issue.package_id}”会清掉这个读取失败的制品，之后列表里不再出现。`,
                        confirmText: '确认移除',
                        run: () => void performPackageAction(`remove:${issue.package_id}`, () => onRemove(issue.package_id)),
                      });
                    }}
                  >
                    <Trash2 size={14} />
                  </button>
                </div>
              ))}
              {filteredWorkflows.map(item => {
                const live = liveRunsByWorkflow.get(item.package.id);
                // 库列表里的「运行中」也要走动：只写状态是一张静止快照，加上已运行时长才有活着的证据。
                const liveSeconds = live && live.startedAt !== null
                  ? Math.max(0, Math.floor(nowMs / 1000) - live.startedAt)
                  : null;
                return (
                <div
                  key={item.package.id}
                  className={[
                    'workflow-list-item',
                    selectedWorkflow?.package.id === item.package.id ? 'active' : '',
                    item.enabled ? '' : 'is-disabled',
                  ].filter(Boolean).join(' ')}
                >
                  <button
                    type="button"
                    className={live ? 'is-live' : ''}
                    onClick={() => setSelectedWorkflowId(item.package.id)}
                  >
                    <span className="workflow-list-mark"><Workflow size={16} /></span>
                    <span>
                      <strong>{item.package.name}</strong>
                      <small>v{item.package.version} · {item.package.steps.length} 步</small>
                      {live ? (
                        <small className="workflow-list-live" title={live.label ? `${live.label} · ${live.runId}` : live.runId}>
                          {live.count > 1 ? `${live.count} 个运行中` : '运行中'}
                          {liveSeconds !== null ? ` · 已运行 ${formatElapsedCn(liveSeconds)}` : live.label ? ` · ${live.label}` : ''}
                        </small>
                      ) : null}
                    </span>
                  </button>
                  {/* 行尾开关是工作流唯一的启停控件，不再是「已启用」胶囊：默认启用是常态，
                      4 行全挂一枚 62px 的「已启用」等于占着第三列说同一句默认值，还把名字
                      两行挤到 151px。状态与动作合成同一个控件，和「定时计划」列表同一套行结构。
                      详情头部原来还有一枚同样的开关，现在是第二次说同一句话，已删掉——
                      同一屏上一个布尔值只留一个控件，详情只读状态（标题旁的状态胶囊）。
                      停用的连带后果写进 title：停用后不能运行，指向它的定时计划也不会执行。 */}
                  <label className="toggle compact workflow-quick-toggle" title={item.enabled ? '停用工作流（停用后不能运行，指向它的定时计划也不会执行）' : '启用工作流（启用后可以运行，也可以被定时计划调用）'}>
                    <input
                      type="checkbox"
                      checked={item.enabled}
                      disabled={Boolean(packageBusy)}
                      aria-label={`${item.enabled ? '停用' : '启用'}工作流 ${item.package.name}`}
                      onChange={() => void performPackageAction(
                        `toggle:${item.package.id}`,
                        () => onSetEnabled(item.package.id, !item.enabled),
                      )}
                    />
                    <span className="slider" />
                  </label>
                </div>
                );
              })}
              {!loading && workflows.length > 0 && filteredWorkflows.length === 0 && visibleLibraryIssues.length === 0 ? (
                <div className="workflow-library-empty">
                  <EmptyState icon={Search} title="没有匹配的工作流" text="换一个名称、ID 或状态试试。" />
                  <button type="button" className="btn" onClick={() => { setLibrarySearch(''); setLibraryStatusFilter('all'); }}>清除筛选</button>
                </div>
              ) : null}
              {!loading && workflows.length === 0 && libraryIssues.length === 0 ? (
                /* 空库时整屏只留一个空态：结论与「下一步」都放在右侧详情区，
                   窄列表这里只用一行浅色文字说明这一列为什么是空的。 */
                <p className="workflow-library-hint">暂无已安装的工作流。</p>
              ) : null}
            </div>
          </>}
        </section>
        <section className="card workflow-detail-panel">
          {!showLibrary ? (
            /* hero 是这次运行的身份与状态，按「身份 + 状态 + 对象级动作留在头部、
               正文单独滚动」的口径冻结在卡片上：运行中唯一的动作「取消运行」跟在
               状态后面，不用翻到正文最底部去找。 */
            <>
            {!runLoading && runDetail ? (
              <div className="workflow-run-head">
                <div className={`workflow-run-hero ${timeline?.active ? 'is-live' : ''}`}>
                  <div>
                    <span>{runDetail.workflow?.package.id || runDetail.run.run_id}</span>
                    <h2>{runDetail.workflow?.package.name || '工作流运行'}</h2>
                    <p>{timeline?.headline || statusLabel(runDetail.run.status)}</p>
                  </div>
                  <div className="workflow-run-hero-side">
                    {/* 状态点只留一套：状态胶囊自带圆点，这里不再另加跳动圆点——
                        同一状态并排两个点（蓝点 + 胶囊内的黄点）是重复表达。
                        运行中的持续反馈由胶囊颜色、页头「1 个运行中」和右侧运行指标承担。 */}
                    <Pill kind={statusKind(runDetail.run.status)}>{runDetail.run.status === 'waiting' ? waitingKindLabel(currentWaitingKind) : statusLabel(runDetail.run.status)}</Pill>
                    {['queued', 'running', 'waiting'].includes(runDetail.run.status) ? (
                      <button type="button" className="btn btn-danger-quiet" disabled={Boolean(actionBusy)} onClick={() => void performRunAction(runDetail.run.run_id, 'cancel', () => onCancel(runDetail.run.run_id))}>{actionBusy === 'cancel' ? '正在取消' : '取消运行'}</button>
                    ) : null}
                  </div>
                </div>
              </div>
            ) : null}
            <div className="workflow-detail-body">
            {runLoading ? <div className="page-loading"><BusyIndicator size={15} />正在读取运行详情</div>
              : runDetail ? (
              <>
                {runError ? <div className="workflow-inline-error">{runError}</div> : null}
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
                      <div><span>运行号</span><strong>{shortRunId(runDetail.run.run_id)}</strong></div>
                    </div>
                    <div className={`workflow-run-progress${!timeline.active && timeline.percent === 100 ? ' is-done' : ''}`} role="progressbar" aria-valuenow={timeline.percent} aria-valuemin={0} aria-valuemax={100} aria-label="步骤完成度">
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
                      {actionBusy === 'resume' ? <BusyIndicator size={14} /> : <CheckCircle2 size={14} />}{actionBusy === 'resume' ? '提交中' : '提交反馈并继续'}
                    </button>
                  </section>
                ) : waitingForApproval ? (
                  <section className="workflow-action-card approval">
                    <div><ShieldCheck size={18} /><div><strong>{waitingRequest?.title || currentStepDefinition?.title || '等待审批'}</strong><span>{approvalDescription(currentStepDefinition, runDetail.run, waitingRequest)}</span>{waitingRequest?.risk_level ? <small>{riskLevelLabel(waitingRequest.risk_level)}风险</small> : null}{waitingRequest?.schema ? <details className="workflow-interaction-schema"><summary>查看提交格式</summary><pre>{JSON.stringify(waitingRequest.schema, null, 2)}</pre></details> : null}</div></div>
                    <div className="workflow-run-actions">
                      <button type="button" className="btn btn-primary" disabled={Boolean(actionBusy)} onClick={() => void performRunAction(runDetail.run.run_id, 'approve', () => onApprove(runDetail.run.run_id, runDetail.run.current_step_id))}>{actionBusy === 'approve' ? <BusyIndicator size={14} /> : <CheckCircle2 size={14} />}{actionBusy === 'approve' ? '处理中' : '批准并继续'}</button>
                      <button type="button" className="btn" disabled={Boolean(actionBusy)} onClick={() => void performRunAction(runDetail.run.run_id, 'reject', () => onReject(runDetail.run.run_id, runDetail.run.current_step_id))}>拒绝</button>
                    </div>
                  </section>
                ) : waitingForExternal || waitingForOtherInteraction ? (
                  <section className="workflow-action-card external-wait">
                    <div><Clock3 size={18} /><div><strong>{waitingRequest?.title || '等待外部状态或人工操作'}</strong><span>{waitingRequest?.description || '这里暂时没有可执行操作，请查看任务动态和外部系统状态。'}</span><small>{waitingRequest?.required_action ? interactionActionLabel(waitingRequest.required_action) : '检查运行状态'}</small>{waitingRequest?.schema ? <details className="workflow-interaction-schema"><summary>查看提交格式</summary><pre>{JSON.stringify(waitingRequest.schema, null, 2)}</pre></details> : null}</div></div>
                    <div className="workflow-run-actions"><button type="button" className="btn" onClick={() => void openRun(runDetail.run.run_id)}><RefreshCw size={14} />查看最新状态</button></div>
                  </section>
                ) : null}
                <div className="workflow-run-context">
                  <div><span>工作流</span><strong>{runDetail.workflow?.package.name || selectedWorkflow?.package.name || '未记录'}</strong></div>
                  <div><span>当前步骤</span><strong>{currentStepDefinition?.title || runDetail.run.current_step_id || '已完成'}</strong></div>
                  <div><span>运行环境</span><strong>{runtimeProviderLabel(runDetail.run.runtime_provider, runtimeProfiles)}</strong></div>
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
                {runDetail.run.status === 'succeeded' ? <section className="workflow-section"><button type="button" className="btn" disabled={Boolean(actionBusy)} onClick={() => void verifyRun(runDetail.run.run_id)}>{actionBusy === 'verify' ? <BusyIndicator size={14} /> : <ShieldCheck size={14} />}{actionBusy === 'verify' ? '验证中' : '验证结果'}</button></section> : null}
                {runVerification ? (
                  <section className="workflow-verification">
                    <div><ShieldCheck size={16} /><span><strong>验证通过</strong><small>{runVerification.candidate_id.slice(0, 16)} · {runVerification.commit_sha.slice(0, 12)} · {runVerification.signature_key_id || '未签名'}</small></span></div>
                    {runVerification.artifacts.map(artifact => <div key={artifact.artifact_id}><span><strong>{artifact.artifact_id}</strong><small>{artifact.sha256.slice(0, 16)} · {formatBytes(artifact.size_bytes)}</small></span><Pill kind={artifact.candidate_bound ? 'success' : 'neutral'}>{artifact.candidate_bound ? '已验证' : '未绑定'}</Pill></div>)}
                  </section>
                ) : null}
              </>
            ) : runError ? <div className="blocker"><CircleAlert size={18} /><div><strong>运行详情读取失败</strong><span>{runError}</span></div></div>
              : <EmptyState icon={Clock3} title="选择运行任务" text="左侧优先显示等待反馈、等待审批和运行中的任务。" />
            }
            </div>
            </>
          ) : selectedWorkflow ? (
            <>
              <div className="workflow-detail-head">
                <div className="workflow-detail-title">
                  <ExtensionKindMark kind="workflow" />
                  <div>
                    <span>工作流</span>
                    {/* 状态跟着标题走：与技能、插件详情同一个位置。 */}
                    <div className="workflow-title-line">
                      <h2>{selectedWorkflow.package.name}</h2>
                      <Pill kind={selectedWorkflow.enabled ? 'success' : 'neutral'}>{selectedWorkflow.enabled ? '已启用' : '已停用'}</Pill>
                    </div>
                    <p>{selectedWorkflow.package.description}</p>
                    {/* 停用说明放在状态这一侧（描述下面），不放进动作区：
                        动作区只有 666px 一行，多出 200px 的说明会把三枚按钮挤到第二行，
                        于是「启用/停用」一切换，按钮就上下跳一行。这里只在停用时多一行，
                        按钮位置两种状态都一样。 */}
                    {selectedWorkflow.enabled ? null : (
                      <div className="workflow-disabled-note">
                        <CircleAlert size={13} aria-hidden="true" />已停用 · 打开左侧列表的开关后可以运行
                      </div>
                    )}
                  </div>
                </div>
                <div className="workflow-detail-actions">
                  {/* 详情只留动作：运行 / 加定时计划 / 移除。
                      启停不在这里：列表行尾那枚开关已经是这个状态的唯一控件，同一屏上左右
                      再各挂一枚，说的是同一个布尔值，用户看不出该按哪一个，一处改完另一处
                      也要跟着动才算对（原来这里还有一枚「启用 + 开关」）。
                      停用的原因与出路写在标题那一侧，这里保持「只有动作」。 */}
                  <button
                    type="button"
                    className="btn btn-primary"
                    title={selectedWorkflow.enabled ? '选一套启动参数后运行' : '工作流已停用，先在左侧列表打开开关'}
                    disabled={!selectedWorkflow.enabled}
                    onClick={() => openStart()}
                  >
                    <Play size={14} />运行
                  </button>
                  <button
                    type="button"
                    className="btn"
                    title="到「定时计划」为这个工作流建立计划"
                    onClick={() => onScheduleWorkflow(selectedWorkflow.package.id)}
                  >
                    <CalendarClock size={14} />加定时计划
                  </button>
                  <button
                    type="button"
                    className="btn btn-danger-quiet"
                    title="移除工作流"
                    disabled={Boolean(packageBusy)}
                    onClick={() => {
                      setPendingConfirm({
                        title: '确认移除工作流？',
                        description: `移除“${selectedWorkflow.package.name}”后它不再出现在这里，指向它的定时计划也会失效；需要时可从「市场」重装。`,
                        confirmText: '确认移除',
                        run: () => void performPackageAction('remove', () => onRemove(selectedWorkflow.package.id)),
                      });
                    }}
                  >
                    <Trash2 size={14} />移除
                  </button>
                </div>
              </div>
              {/* 头部（身份 + 状态 + 动作）留在卡片上不滚，正文单独滚动：
                  这一块内容有 1900px 上下，整卡滚动会把「运行」一起滚出视野
                  （实测滚到底部时动作行 top = -1050px），主操作就够不着了。 */}
              <div className="workflow-detail-body">
              <div className="workflow-meta-grid">
                {/* 详情页只报当前版本：换版本是「市场」里的心智（在市场详情里看历史版本、选一个装上），
                    所以这里不提供回滚入口，避免同一个动作在两个地方各有一套说法。 */}
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
                  <strong>启动方案</strong>
                  <div className="workflow-section-tools">
                    {presets.length ? <span>{presets.length} 套</span> : null}
                    <button type="button" className="btn btn-quiet" onClick={() => openManage()} disabled={!selectedWorkflow.enabled}>管理方案</button>
                  </div>
                </div>
                {presetError ? <div className="workflow-inline-error">{presetError}</div> : null}
                {presets.length ? (
                  <div className="workflow-preset-list">
                    {presets.slice(0, PRESET_INLINE_LIMIT).map(preset => {
                      const name = preset.label || preset.id;
                      const tags = presetTags(preset);
                      return (
                        <div key={preset.id} className={selectedWorkflow.enabled ? 'workflow-preset-row' : 'workflow-preset-row is-disabled'}>
                          {/*
                            整行就是「用这套方案启动」：点开启动弹窗，停在这套参数的确认页。
                            这一列只负责「跑」，改参数、改名、删除、钉常用都在「管理方案」里做 ——
                            详情页是随手开跑的地方，不是编辑台。
                          */}
                          <button
                            type="button"
                            className="workflow-preset-launch"
                            disabled={!selectedWorkflow.enabled || Boolean(presetBusy)}
                            title={`用「${name}」启动 · ${presetSummary(preset, { full: true })}`}
                            onClick={() => openStart(preset)}
                          >
                            <span className="workflow-preset-play" aria-hidden="true"><Play size={12} /></span>
                            <span className="workflow-preset-main">
                              <strong>
                                <span className="workflow-preset-name-text">{name}</span>
                                {preset.pinned ? <em className="workflow-preset-pin-badge" title="已置顶，排在列表最前面">常用</em> : null}
                              </strong>
                              <span className="workflow-preset-tags">
                                {tags.workspaceLabel ? (
                                  <b title={tags.workspace || '未指定工作区'}>{tags.workspace ? <FolderOpen size={11} /> : null}{tags.workspaceLabel}</b>
                                ) : null}
                                {tags.params ? <b>{tags.params}</b> : null}
                                {tags.entrypoint ? <b>入口 {tags.entrypoint}</b> : null}
                                {tags.exitpoint ? <b>出口 {tags.exitpoint}</b> : null}
                              </span>
                            </span>
                          </button>
                        </div>
                      );
                    })}
                    {presets.length > PRESET_INLINE_LIMIT ? (
                      <button type="button" className="workflow-preset-more" onClick={() => openStart()}>
                        还有 {presets.length - PRESET_INLINE_LIMIT} 套方案
                        <ChevronRight size={13} />
                      </button>
                    ) : null}
                  </div>
                ) : (
                  <div className="workflow-start-empty">
                    还没有方案。手动填一次参数并存下来，之后就能一键启动。
                  </div>
                )}
              </div>
              <div className="workflow-section workflow-graph-section">
                <div className="workflow-section-title">
                  <strong><GitBranch size={15} />执行结构</strong>
                  <span>{selectedWorkflowGraph?.nodes.length || 0} 个步骤 · {executionPolicyLabel(selectedWorkflow.package.execution_policy)}</span>
                </div>
                {/* 与上面的「运行指标」用同一套格子：标签在上、数值在下，数值和单位不会被拆成两行。 */}
                <div className="workflow-meta-grid workflow-graph-summary">
                  <div><span>入口</span><strong>{selectedWorkflowGraph?.entrypoints.length || 0}</strong></div>
                  <div><span>出口</span><strong>{selectedWorkflowGraph?.exits.length || 0}</strong></div>
                  <div><span>循环</span><strong>{selectedWorkflowGraph?.nodes.filter(node => node.loop).length || 0}</strong></div>
                  <div><span>输出文件</span><strong>{selectedWorkflow.package.artifacts.length}</strong></div>
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
              </div>
            </>
                ) : !workflows.length && !libraryIssues.length ? (
                  /* 一个都没装时，右侧就是唯一的下一步：不写字面意义上的「请选择」，
                     而是在用户注意力最大的区域直接给出安装入口。 */
                  <div className="workflow-detail-body workflow-detail-empty">
                  <EmptyState icon={Workflow} title="还没有安装工作流" text="到「市场」里浏览并安装，装好后就能在这里运行。" />
                    <button type="button" className="btn btn-primary" onClick={onOpenExtensions}><Store size={14} />去市场</button>
                  </div>
                ) : <div className="workflow-detail-body"><EmptyState
                  icon={Workflow}
                  title={workflows.length && !filteredWorkflows.length ? '当前筛选下没有工作流' : '请选择工作流'}
                  text={workflows.length && !filteredWorkflows.length ? '换一个关键词或状态，也可以选回「全部」。' : '左侧列表用于查看步骤和输出文件。'}
                /></div>}
        </section>
      </div>
      {startOpen && selectedWorkflow ? (
        <div className="modal-backdrop" role="presentation" onClick={event => { if (event.currentTarget === event.target) setStartOpen(false); }}>
          <section className="modal workflow-start-modal" role="dialog" aria-modal="true" aria-labelledby="workflow-start-title">
            <div className="workflow-start-head">
              <div>
                <span>{startStep === 'choose' ? '选择启动方案' : '启动运行'}</span>
                <h2 id="workflow-start-title">{selectedWorkflow.package.name}</h2>
              </div>
              <button type="button" className="btn btn-icon" title="关闭" aria-label="关闭" onClick={() => setStartOpen(false)}><X size={16} /></button>
            </div>
            {startStep === 'choose' ? (
              <div className="workflow-start-choose">
                <div className="workflow-start-choose-head">
                  <strong>用哪套方案启动</strong>
                  <small>{presetsLoading ? '正在读取方案…' : `${presets.length} 套可用方案`}</small>
                </div>
                {presetError ? <div className="workflow-inline-error" role="alert">{presetError}</div> : null}
                {presets.length > PRESET_SEARCH_LIMIT ? (
                  <label className="workflow-start-search">
                    <Search size={14} />
                    <input
                      type="search"
                      value={presetQuery}
                      placeholder="搜索方案名称或工作区"
                      aria-label="搜索方案"
                      autoFocus
                      onChange={event => setPresetQuery(event.target.value)}
                    />
                  </label>
                ) : null}
                {/* 行内「运行」是给已经确定的场景省一次点击；点行本身进参数步骤，先看再跑。 */}
                <div className="workflow-preset-picker" role="list" aria-label="启动方案" onKeyDown={handlePickerKeyDown}>
                  {visiblePresets.map(preset => {
                    const name = preset.label || preset.id;
                    const tags = presetTags(preset);
                    const running = presetBusy === `run:${preset.id}`;
                    // 常用优先，其次是「最近动过的那一套」：两个徽标都只在解释排序，不参与点击。
                    const badge = preset.pinned ? '常用' : (preset.id === recentPresetId ? '最近保存' : '');
                    return (
                      <div
                        key={preset.id}
                        role="listitem"
                        tabIndex={0}
                        data-preset-row
                        className="workflow-preset-option"
                        onClick={() => chooseStartPreset(preset)}
                        onKeyDown={event => {
                          if (event.key === 'Enter' || event.key === ' ') {
                            event.preventDefault();
                            chooseStartPreset(preset);
                          }
                        }}
                      >
                        <span className="workflow-preset-option-main">
                          <strong>
                            {name}
                            {!presetQuery && presets.length > 1 && badge ? (
                              <em
                                className={preset.pinned ? 'workflow-preset-recent workflow-preset-recent-pin' : 'workflow-preset-recent'}
                                title={preset.pinned ? '已置顶，排在列表最前面' : '最近保存的一套'}
                              >
                                {preset.pinned ? <Pin size={10} /> : null}
                                {badge}
                              </em>
                            ) : null}
                          </strong>
                          <span className="workflow-preset-tags" title={presetSummary(preset, { full: true })}>
                            {tags.workspaceLabel ? (
                              <b title={tags.workspace || '未指定工作区'}>{tags.workspace ? <FolderOpen size={11} /> : null}{tags.workspaceLabel}</b>
                            ) : null}
                            {tags.params ? <b>{tags.params}</b> : null}
                            {tags.entrypoint ? <b>入口 {tags.entrypoint}</b> : null}
                            {tags.exitpoint ? <b>出口 {tags.exitpoint}</b> : null}
                          </span>
                        </span>
                        <button
                          type="button"
                          className="btn btn-primary workflow-preset-option-run"
                          disabled={!selectedWorkflow.enabled || Boolean(presetBusy)}
                          title={`直接按「${name}」运行`}
                          onClick={event => { event.stopPropagation(); void runPreset(preset); }}
                        >
                          {running ? <BusyIndicator size={14} /> : <Play size={14} />}{running ? '启动中' : '运行'}
                        </button>
                      </div>
                    );
                  })}
                  {!visiblePresets.length ? (
                    <div className="workflow-start-empty">{presetQuery ? '没有匹配的方案。' : '还没有方案；填完参数存下来，下次就能一键启动。'}</div>
                  ) : null}
                </div>
                {/* 没有方案时这一行是这一步唯一可做的事：给它主操作的分量，不让眼睛空转。 */}
                <button
                  type="button"
                  className={presets.length ? 'workflow-preset-manual' : 'workflow-preset-manual is-primary'}
                  onClick={startFromScratch}
                >
                  <Plus size={14} />
                  <span>
                    <strong>不用方案，手动填写参数</strong>
                    <small>只跑这一次，参数不保存；需要复用就在下一步存成方案</small>
                  </span>
                </button>
              </div>
            ) : <>
            <div className="workflow-start-params-head">
              <div>
                <strong>{startPreset ? `方案「${startPreset.label || startPreset.id}」` : '手动填写参数'}</strong>
                <small>{startPreset ? '参数来自这套方案，改动后可更新或另存' : '这次不用方案；需要复用就存成方案'}</small>
              </div>
              <div className="workflow-start-params-tools">
                <button type="button" className="btn" onClick={() => setStartEdit(current => !current)}>{startEdit ? '收起参数' : '调整参数'}</button>
                {startPreset ? (
                  <button type="button" className="btn" disabled={startBusy || Boolean(presetBusy)} onClick={() => void updateCurrentPreset()}>
                    {presetBusy === 'update' ? '更新中' : '更新方案'}
                  </button>
                ) : null}
                <button
                  type="button"
                  className="btn"
                  disabled={startBusy || Boolean(presetBusy)}
                  onClick={() => {
                    setSaveAsOpen(true);
                    setPresetLabel(startPreset ? `${startPreset.label || startPreset.id} 副本` : '');
                  }}
                >
                  {startPreset ? '另存为…' : '存为方案…'}
                </button>
              </div>
            </div>
            {saveAsOpen ? (
              <div className="workflow-start-preset-row">
                <input
                  type="text"
                  value={presetLabel}
                  placeholder="方案名称（例如：项目看板）"
                  aria-label="方案名称"
                  autoFocus
                  onChange={event => setPresetLabel(event.target.value)}
                />
                <button type="button" className="btn btn-primary" disabled={startBusy || presetBusy === 'save'} onClick={() => void saveStartAsPreset()}>
                  {presetBusy === 'save' ? '保存中' : '保存'}
                </button>
                <button type="button" className="btn" onClick={() => { setSaveAsOpen(false); setPresetLabel(''); }}>取消</button>
              </div>
            ) : null}
            {startEdit ? <>
            <div className="workflow-start-mode segmented-control" role="group" aria-label="输入模式">
              <button type="button" className={startMode === 'form' ? 'active' : ''} onClick={() => switchStartMode('form')}>表单</button>
              <button type="button" className={startMode === 'json' ? 'active' : ''} onClick={() => switchStartMode('json')}>JSON</button>
            </div>
            {startMode === 'form' ? (
              <StartFields
                sections={startSections}
                values={startForm}
                errors={startFieldErrors}
                onChange={setStartField}
                onPickDirectory={fieldId => void pickDirectory(fieldId)}
                containerRef={startFormRef}
              />
            ) : (
              <textarea className="workflow-start-json" value={startInput} onChange={event => { setStartInput(event.target.value); setPreflight(null); }} spellCheck={false} />
            )}
            </> : (
              <div className="workflow-start-summary">
                <div><span>工作区</span><strong title={startSummary.workspace}>{startSummary.workspace || '未指定'}</strong></div>
                {startPreset?.entrypoint ? <div><span>入口</span><strong>{startPreset.entrypoint}</strong></div> : null}
                {startPreset?.exitpoint ? <div><span>出口</span><strong>{startPreset.exitpoint}</strong></div> : null}
                <div>
                  <span>参数项</span>
                  <strong>{startSummary.filled}/{startFields.length} 已填</strong>
                </div>
              </div>
            )}
            </>}
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
                  <BusyIndicator size={13} />
                  正在校验并提交运行…
                </div>
              ) : null}
              {startStep === 'choose' ? (
                <div className="workflow-start-actions">
                  <div>
                    <button type="button" className="btn btn-quiet" onClick={() => openManage()}>管理方案</button>
                  </div>
                  <div>
                    <button type="button" className="btn" onClick={() => setStartOpen(false)}>取消</button>
                  </div>
                </div>
              ) : (
                <div className="workflow-start-actions">
                  <div>
                    {presets.length ? (
                      <button type="button" className="btn btn-quiet" onClick={() => { setStartStep('choose'); setSaveAsOpen(false); }}><ArrowLeft size={14} />换一套方案</button>
                    ) : null}
                  </div>
                  <div>
                    <button type="button" className="btn" onClick={() => setStartOpen(false)}>取消</button>
                    <button type="button" className="btn" disabled={startBusy || preflightBusy} onClick={() => void runPreflight()}>{preflightBusy ? <><BusyIndicator size={14} />检查中</> : '启动前检查'}</button>
                    <button
                      type="button"
                      className="btn btn-primary"
                      title={preflight && !preflight.ready ? preflightNote(preflight, '启动前检查未通过') : undefined}
                      disabled={startBusy || preflightBusy || Boolean(preflight && !preflight.ready)}
                      onClick={() => void submitStart()}
                    >
                      {startBusy ? <BusyIndicator size={14} /> : <Play size={14} />}{startBusy ? '正在启动' : '启动运行'}
                    </button>
                  </div>
                </div>
              )}
            </div>
          </section>
        </div>
      ) : null}
      {manageOpen && selectedWorkflow && manageDraft ? (
        <div className="modal-backdrop" role="presentation" onClick={event => { if (event.currentTarget === event.target) setManageOpen(false); }}>
          <section className="modal workflow-preset-manager" role="dialog" aria-modal="true" aria-labelledby="workflow-preset-manager-title">
            <div className="workflow-start-head">
              <div>
                <span>方案管理</span>
                <h2 id="workflow-preset-manager-title">{selectedWorkflow.package.name}</h2>
              </div>
              <button type="button" className="btn btn-icon" title="关闭" aria-label="关闭" onClick={() => setManageOpen(false)}><X size={16} /></button>
            </div>
            <div className="workflow-preset-manager-body">
              <div className="workflow-preset-manager-list">
                <div className="workflow-preset-manager-list-head">
                  <strong>方案</strong>
                  <Pill kind="neutral">{presets.length}</Pill>
                  <button type="button" className="btn btn-icon" title="新建方案" aria-label="新建方案" onClick={newManageDraft}><Plus size={14} /></button>
                </div>
                {presets.length > PRESET_SEARCH_LIMIT ? (
                  <label className="workflow-start-search">
                    <Search size={14} />
                    <input
                      type="search"
                      value={manageQuery}
                      placeholder="搜索方案"
                      aria-label="搜索方案"
                      onChange={event => setManageQuery(event.target.value)}
                    />
                  </label>
                ) : null}
                <div className="workflow-preset-manager-items">
                  {managePresets.map(preset => {
                    const name = preset.label || preset.id;
                    const tags = presetTags(preset);
                    return (
                      <div
                        key={preset.id}
                        className={preset.id === manageDraft.id ? 'workflow-preset-manager-item active' : 'workflow-preset-manager-item'}
                      >
                        <button
                          type="button"
                          className="workflow-preset-manager-pick"
                          onClick={() => {
                            setManageDraft(draftFromPreset(preset, normalizedStartFields(selectedWorkflow)));
                            setManageErrors({});
                            setManageMessage('');
                          }}
                        >
                          <strong>{name}</strong>
                          <small title={tags.workspace || tags.workspaceLabel}>{[tags.workspaceLabel, tags.params].filter(Boolean).join(' · ') || '—'}</small>
                        </button>
                        {/*
                          置顶是这一套方案自己的开关，点一下立刻落库：常用方案要在「启动」列表里
                          排到最前面，改这个位不该还要用户先选中、再另存一次。
                        */}
                        <button
                          type="button"
                          className={preset.pinned ? 'workflow-preset-manager-pin is-on' : 'workflow-preset-manager-pin'}
                          title={preset.pinned ? '取消置顶' : '置顶到列表最前面'}
                          aria-label={preset.pinned ? `取消置顶「${name}」` : `置顶「${name}」`}
                          aria-pressed={Boolean(preset.pinned)}
                          disabled={Boolean(pinBusy)}
                          onClick={() => void togglePresetPin(preset)}
                        >
                          <Pin size={13} />
                        </button>
                      </div>
                    );
                  })}
                  {!managePresets.length ? (
                    <small className="workflow-preset-manager-hint">{presets.length ? '没有匹配的方案。' : '还没有方案，点右上角 + 新建。'}</small>
                  ) : null}
                </div>
              </div>
              <div className="workflow-preset-manager-form">
                <label className="workflow-preset-name">
                  <span>
                    名称
                    {!manageDraft.id ? <em>新方案</em> : null}
                    {manageDraft.id && manageDraft.pinned ? <em className="workflow-preset-pin-badge" title="已置顶，排在列表最前面">常用</em> : null}
                  </span>
                  <input
                    type="text"
                    value={manageDraft.label}
                    placeholder="例如：项目看板"
                    aria-label="方案名称"
                    onChange={event => setManageDraft(current => current ? { ...current, label: event.target.value } : current)}
                  />
                </label>
                <StartFields
                  sections={startSections}
                  values={manageDraft.values}
                  errors={manageErrors}
                  onChange={setManageField}
                  onPickDirectory={fieldId => { void onPickDirectory().then(path => { if (path) setManageField(fieldId, path); }); }}
                />
                {(selectedWorkflowGraph?.entrypoints.length || selectedWorkflowGraph?.exits.length) ? (
                  <div className="workflow-preset-endpoints">
                    <label>
                      <span>入口</span>
                      <select
                        value={manageDraft.entrypoint}
                        onChange={event => setManageDraft(current => current ? { ...current, entrypoint: event.target.value } : current)}
                      >
                        <option value="">跟随工作流默认</option>
                        {(selectedWorkflowGraph?.entrypoints || []).map(endpoint => (
                          <option key={endpoint.id} value={endpoint.id}>{endpoint.label} · {endpoint.atStep}</option>
                        ))}
                      </select>
                    </label>
                    <label>
                      <span>出口</span>
                      <select
                        value={manageDraft.exitpoint}
                        onChange={event => setManageDraft(current => current ? { ...current, exitpoint: event.target.value } : current)}
                      >
                        <option value="">跟随工作流默认</option>
                        {(selectedWorkflowGraph?.exits || []).map(endpoint => (
                          <option key={endpoint.id} value={endpoint.id}>{endpoint.label} · {endpoint.atStep}</option>
                        ))}
                      </select>
                    </label>
                  </div>
                ) : null}
              </div>
            </div>
            <div className="workflow-start-footer">
              {manageMessage ? <div className="workflow-preset-manager-message" role="status">{manageMessage}</div> : null}
              <div className="workflow-start-actions">
                <div>
                  <button
                    type="button"
                    className="btn btn-danger-quiet"
                    disabled={!manageDraft.id || Boolean(manageBusy)}
                    onClick={() => setPendingConfirm({
                      title: '确认删除启动方案？',
                      description: `删除「${manageDraft.label || manageDraft.id}」后，引用它的定时计划会因方案缺失无法启动。`,
                      confirmText: '确认删除',
                      run: () => void deleteManageDraft(),
                    })}
                  >
                    {manageBusy === 'delete' ? '删除中' : '删除'}
                  </button>
                  <button type="button" className="btn" disabled={Boolean(manageBusy)} onClick={duplicateManageDraft}>复制</button>
                </div>
                <div>
                  <button type="button" className="btn" onClick={() => setManageOpen(false)}>关闭</button>
                  <button type="button" className="btn btn-primary" disabled={Boolean(manageBusy)} onClick={() => void saveManageDraft()}>
                    {manageBusy === 'save' ? '保存中' : '保存'}
                  </button>
                </div>
              </div>
            </div>
          </section>
        </div>
      ) : null}
      {/* 站内确认弹窗：和插件页「卸载 / 停用」同一套 .modal 结构，标题说清动作，
          说明写清后果，确认按钮用危险色。原生 confirm 不在这里出现。 */}
      {pendingConfirm ? (
        <div className="modal-backdrop" role="presentation" onClick={event => { if (event.currentTarget === event.target) setPendingConfirm(null); }}>
          <section className="modal workflow-confirm-modal" role="dialog" aria-modal="true" aria-labelledby="workflow-confirm-title">
            <div className="modal-header">
              <div>
                <h3 id="workflow-confirm-title">{pendingConfirm.title}</h3>
                <p>{pendingConfirm.description}</p>
              </div>
              <button type="button" className="btn btn-icon" title="关闭" aria-label="关闭" onClick={() => setPendingConfirm(null)}><X size={16} /></button>
            </div>
            <div className="modal-body">
              <div className="modal-actions">
                <button type="button" className="btn" onClick={() => setPendingConfirm(null)}>取消</button>
                <button
                  type="button"
                  className="btn btn-danger"
                  onClick={() => {
                    const action = pendingConfirm;
                    setPendingConfirm(null);
                    action.run();
                  }}
                >
                  {pendingConfirm.confirmText}
                </button>
              </div>
            </div>
          </section>
        </div>
      ) : null}
    </div>
  );
}
