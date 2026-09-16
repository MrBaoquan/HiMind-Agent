import { useEffect, useMemo, useState } from 'react';
import { CheckCircle2, Clock3, FileText, RefreshCw, ShieldCheck, Workflow } from 'lucide-react';
import { EmptyState, PageHeader, Pill } from '../components/Common';
import type { WorkflowCenterSnapshot, WorkflowLocalRun, WorkflowRunSnapshot } from '../services/agentApi';

type WorkflowsPageProps = {
  snapshot: WorkflowCenterSnapshot | null;
  loading: boolean;
  error: string;
  onRefresh: () => void;
  onLoadRun: (runId: string) => Promise<WorkflowRunSnapshot>;
  onApprove: (runId: string, stepId: string) => Promise<void>;
  onReject: (runId: string, stepId: string) => Promise<void>;
  onCancel: (runId: string) => Promise<void>;
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

export function WorkflowsPage({ snapshot, loading, error, onRefresh, onLoadRun, onApprove, onReject, onCancel }: WorkflowsPageProps) {
  const [selectedWorkflowId, setSelectedWorkflowId] = useState('');
  const [selectedRunId, setSelectedRunId] = useState('');
  const [runDetail, setRunDetail] = useState<WorkflowRunSnapshot | null>(null);
  const [runLoading, setRunLoading] = useState(false);
  const [runError, setRunError] = useState('');
  const [actionBusy, setActionBusy] = useState('');

  const workflows = snapshot?.workflows || [];
  const runs = snapshot?.runs || [];
  const selectedWorkflow = useMemo(
    () => workflows.find(item => item.package.id === selectedWorkflowId) || workflows[0] || null,
    [selectedWorkflowId, workflows],
  );

  useEffect(() => {
    if (!selectedWorkflowId && workflows[0]) setSelectedWorkflowId(workflows[0].package.id);
  }, [selectedWorkflowId, workflows]);

  async function openRun(runId: string) {
    setSelectedRunId(runId);
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
      setRunError('工作流操作失败，请查看 Agent 日志后重试');
    } finally {
      setActionBusy('');
    }
  }

  const waiting = runs.filter(item => item.run.status === 'waiting').length;
  const succeeded = runs.filter(item => item.run.status === 'succeeded').length;
  const artifacts = selectedWorkflow?.package.artifacts.length || 0;

  return (
    <div className="workflow-page">
      <PageHeader
        title="工作流"
        description="本地业务包、执行事实和交付产物"
        actions={<button className="btn btn-icon" title="刷新工作流" aria-label="刷新工作流" onClick={onRefresh}><RefreshCw size={16} className={loading ? 'spin' : ''} /></button>}
      />
      {error ? <div className="blocker"><FileText size={18} /><div><strong>工作流数据读取失败</strong><span>{error}</span></div></div> : null}
      <section className="workflow-summary" aria-label="工作流概览">
        <div><Workflow size={18} /><span><small>已安装</small><strong>{workflows.length}</strong></span></div>
        <div><CheckCircle2 size={18} /><span><small>已完成</small><strong>{succeeded}</strong></span></div>
        <div><Clock3 size={18} /><span><small>等待操作</small><strong>{waiting}</strong></span></div>
        <div><FileText size={18} /><span><small>Artifact</small><strong>{artifacts}</strong></span></div>
      </section>
      <div className="workflow-layout">
        <section className="card workflow-list-panel">
          <div className="card-header"><strong>已安装工作流</strong><Pill kind="neutral">{workflows.length}</Pill></div>
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
            {!loading && workflows.length === 0 ? <EmptyState icon={Workflow} title="暂无工作流" text="安装 Workflow Package 后会显示在这里。" /> : null}
          </div>
        </section>
        <section className="card workflow-detail-panel">
          {selectedWorkflow ? (
            <>
              <div className="workflow-detail-head">
                <div>
                  <span>Workflow Package</span>
                  <h2>{selectedWorkflow.package.name}</h2>
                  <p>{selectedWorkflow.package.description}</p>
                </div>
                <Pill kind={selectedWorkflow.enabled ? 'success' : 'neutral'}>{selectedWorkflow.enabled ? '可运行' : '已停用'}</Pill>
              </div>
              <div className="workflow-meta-grid">
                <div><span>版本</span><strong>v{selectedWorkflow.package.version}</strong></div>
                <div><span>UI</span><strong>{selectedWorkflow.package.ui.mode}</strong></div>
                <div><span>Runtime</span><strong>{selectedWorkflow.package.supported_runtimes.length}</strong></div>
                <div><span>摘要</span><code>{selectedWorkflow.package_digest.slice(0, 12)}</code></div>
              </div>
              <div className="workflow-section">
                <div className="workflow-section-title"><strong>执行步骤</strong><span>{selectedWorkflow.package.steps.length} 步</span></div>
                <div className="workflow-step-list">
                  {selectedWorkflow.package.steps.map((step, index) => (
                    <div key={step.id}>
                      <span className="workflow-step-index">{String(index + 1).padStart(2, '0')}</span>
                      <span><strong>{step.title}</strong><small>{step.capability_id || '人工 / Runtime 步骤'}</small></span>
                      {step.approval_required ? <ShieldCheck size={15} aria-label="需要审批" /> : null}
                    </div>
                  ))}
                </div>
              </div>
              <div className="workflow-section">
                <div className="workflow-section-title"><strong>交付 Artifact</strong><span>{selectedWorkflow.package.artifacts.length} 类</span></div>
                <div className="workflow-artifact-list">
                  {selectedWorkflow.package.artifacts.map(artifact => (
                    <div key={artifact.id}><span><strong>{artifact.name}</strong><small>{artifact.artifact_type}</small></span><Pill kind={artifact.required ? 'warn' : 'neutral'}>{artifact.required ? '必需' : '可选'}</Pill></div>
                  ))}
                </div>
              </div>
            </>
          ) : <EmptyState icon={Workflow} title="请选择工作流" text="左侧列表用于查看包结构和交付契约。" />}
        </section>
        <section className="card workflow-runs-panel">
          <div className="card-header"><strong>最近运行</strong><Pill kind="neutral">{runs.length}</Pill></div>
          <div className="workflow-run-list">
            {runs.map(item => (
              <button type="button" key={item.run.run_id} className={selectedRunId === item.run.run_id ? 'active' : ''} onClick={() => openRun(item.run.run_id)}>
                <span><strong>{item.run.run_id}</strong><small>{formatTime(item.run.updated_at)}</small></span>
                <Pill kind={statusKind(item.run.status)}>{statusLabel(item.run.status)}</Pill>
              </button>
            ))}
            {!loading && runs.length === 0 ? <EmptyState icon={Clock3} title="暂无运行记录" text="工作流执行后会保留在本地 Ledger。" /> : null}
          </div>
          <div className="workflow-run-detail">
            {runLoading ? <div className="page-loading"><span className="spinner" />正在读取运行详情</div> : null}
            {runError ? <div className="workflow-inline-error">{runError}</div> : null}
            {runDetail ? (
              <>
                <div className="workflow-run-status">
                  <Pill kind={statusKind(runDetail.run.status)}>{statusLabel(runDetail.run.status)}</Pill>
                  <span>{runDetail.run.artifacts.length} Artifact · {runDetail.events.length} Event</span>
                </div>
                {runDetail.run.status === 'waiting' && runDetail.run.current_step_id ? (
                  <div className="workflow-run-actions">
                    <button
                      type="button"
                      className="btn btn-primary"
                      disabled={Boolean(actionBusy)}
                      onClick={() => void performRunAction(runDetail.run.run_id, 'approve', () => onApprove(runDetail.run.run_id, runDetail.run.current_step_id))}
                    >
                      <CheckCircle2 size={14} />{actionBusy === 'approve' ? '处理中' : '批准并继续'}
                    </button>
                    <button
                      type="button"
                      className="btn"
                      disabled={Boolean(actionBusy)}
                      onClick={() => void performRunAction(runDetail.run.run_id, 'reject', () => onReject(runDetail.run.run_id, runDetail.run.current_step_id))}
                    >
                      拒绝
                    </button>
                  </div>
                ) : null}
                {runDetail.run.status === 'running' || runDetail.run.status === 'queued' || runDetail.run.status === 'waiting' ? (
                  <div className="workflow-run-actions">
                    <button
                      type="button"
                      className="btn btn-danger-quiet"
                      disabled={Boolean(actionBusy)}
                      onClick={() => void performRunAction(runDetail.run.run_id, 'cancel', () => onCancel(runDetail.run.run_id))}
                    >
                      {actionBusy === 'cancel' ? '正在取消' : '取消运行'}
                    </button>
                  </div>
                ) : null}
                <div className="workflow-run-artifacts">
                  {runDetail.run.artifacts.map(artifact => (
                    <div key={artifact.artifact_id}><FileText size={14} /><span><strong>{artifact.name}</strong><small>{artifact.artifact_id}</small></span></div>
                  ))}
                </div>
              </>
            ) : null}
          </div>
        </section>
      </div>
    </div>
  );
}
