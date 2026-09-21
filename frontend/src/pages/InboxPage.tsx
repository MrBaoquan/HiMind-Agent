import { ArrowRight, CheckCircle2, CircleAlert, ClipboardCheck, Clock3, MessageCircle, RefreshCw, ShieldCheck, Workflow, XCircle } from 'lucide-react';
import { EmptyState, IconButton, PageHeader, Pill } from '../components/Common';
import type { ApprovalItem, WorkflowCenterSnapshot } from '../services/agentApi';

type WaitingRun = WorkflowCenterSnapshot['runs'][number];

function waitingLabel(item: WaitingRun) {
  const kind = item.interaction_request?.kind || item.waiting_kind;
  if (kind === 'feedback') return '等待开发反馈';
  if (kind === 'approval') return '等待工作流审批';
  if (kind === 'external_wait') return '等待外部状态';
  if (kind === 'form') return '等待填写信息';
  if (kind === 'evidence') return '等待提交证据';
  return '等待处理';
}

function actionLabel(action?: string) {
  return ({
    approve_or_reject: '批准或拒绝',
    submit_feedback: '提交反馈',
    submit_form: '填写信息',
    submit_evidence: '提交证据',
    inspect_run: '检查运行状态',
  } as Record<string, string>)[action || ''] || action || '';
}

function waitingIcon(item: WaitingRun) {
  const kind = item.interaction_request?.kind || item.waiting_kind;
  if (kind === 'feedback') return <MessageCircle size={18} />;
  if (kind === 'approval') return <ShieldCheck size={18} />;
  return <CircleAlert size={18} />;
}

function formatTime(value: string) {
  if (!value) return '--';
  const numeric = Number(value);
  const date = Number.isFinite(numeric) && numeric > 0 ? new Date(numeric * 1000) : new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString('zh-CN', { hour12: false });
}

export function InboxPage({ approvals, workflowRuns, onRefresh, onRespond, onOpenWorkflowRun, onOpenApprovalHistory }: {
  approvals: ApprovalItem[];
  workflowRuns: WaitingRun[];
  onRefresh: () => void;
  onRespond: (id: string, approved: boolean) => void;
  onOpenWorkflowRun: (runId: string) => void;
  onOpenApprovalHistory: () => void;
}) {
  const total = approvals.length + workflowRuns.length;

  return (
    <>
      <PageHeader
        title="待处理"
        description="集中处理需要你决定、反馈或继续操作的事项。"
        actions={<div className="page-header-actions"><button type="button" className="btn" onClick={onOpenApprovalHistory}><ClipboardCheck size={14} />审批记录</button><IconButton icon={RefreshCw} label="刷新待处理" onClick={onRefresh} /></div>}
      />
      <div className="inbox-summary" aria-label="待处理摘要">
        <div><strong>{total}</strong><span>全部待处理</span></div>
        <div><strong>{approvals.length}</strong><span>操作审批</span></div>
        <div><strong>{workflowRuns.length}</strong><span>工作流等待</span></div>
      </div>
      {total === 0 ? (
        <div className="card inbox-empty-card">
          <EmptyState icon={CheckCircle2} title="当前没有待处理事项" text="新的审批、反馈和人工交互会集中显示在这里。" />
        </div>
      ) : (
        <div className="inbox-grid">
          <section className="card inbox-section">
            <div className="inbox-section-header">
              <div><strong>操作审批</strong><span>需要确认的敏感操作</span></div>
              <Pill kind={approvals.length ? 'warn' : 'neutral'}>{approvals.length}</Pill>
            </div>
            {approvals.length ? (
              <div className="inbox-item-list">
                {approvals.map(item => (
                  <article className="inbox-item" key={item.id}>
                    <div className="inbox-item-icon approval"><ShieldCheck size={18} /></div>
                    <div className="inbox-item-main">
                      <strong>{item.title}</strong>
                      <span>{item.description}</span>
                      <small><Clock3 size={12} />{item.timeout_seconds === 0 ? '等待人工决定' : `剩余 ${item.remaining_seconds ?? item.timeout_seconds ?? 30} 秒`}</small>
                    </div>
                    <div className="inbox-item-actions">
                      <button type="button" className="btn" onClick={() => onRespond(item.id, false)}><XCircle size={14} />拒绝</button>
                      <button type="button" className="btn btn-primary" onClick={() => onRespond(item.id, true)}><CheckCircle2 size={14} />允许</button>
                    </div>
                  </article>
                ))}
              </div>
            ) : <div className="inbox-section-empty">暂无操作审批。</div>}
          </section>

          <section className="card inbox-section">
            <div className="inbox-section-header">
              <div><strong>工作流等待</strong><span>等待反馈、审批或外部操作</span></div>
              <Pill kind={workflowRuns.length ? 'warn' : 'neutral'}>{workflowRuns.length}</Pill>
            </div>
            {workflowRuns.length ? (
              <div className="inbox-item-list">
                {workflowRuns.map(item => (
                  <button type="button" className="inbox-item inbox-item-button" key={item.run.run_id} onClick={() => onOpenWorkflowRun(item.run.run_id)}>
                    <div className="inbox-item-icon workflow">{waitingIcon(item)}</div>
                    <div className="inbox-item-main">
                      <strong>{item.interaction_request?.title || item.workflow_name || item.workflow_id || '工作流运行'}</strong>
                      <span>{waitingLabel(item)}{item.current_step_title ? ` · ${item.current_step_title}` : ''}</span>
                      <small><Workflow size={12} />{item.app_id || item.project_root || item.workspace_root || '未记录项目'} · {formatTime(item.run.updated_at)}</small>
                      {(item.interaction_request?.description || item.waiting_reason) ? <small className="inbox-item-reason">{item.interaction_request?.description || item.waiting_reason}</small> : null}
                      {(item.interaction_request?.required_action || item.required_action) ? <small className="inbox-item-action">{actionLabel(item.interaction_request?.required_action || item.required_action)}</small> : null}
                    </div>
                    <ArrowRight size={17} aria-hidden="true" />
                  </button>
                ))}
              </div>
            ) : <div className="inbox-section-empty">暂无等待中的工作流。</div>}
          </section>
        </div>
      )}
    </>
  );
}
