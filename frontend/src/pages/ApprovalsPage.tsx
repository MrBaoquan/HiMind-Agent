import { useState, type KeyboardEvent } from 'react';
import type { ApprovalFact, ApprovalItem } from '../services/agentApi';
import { CheckCircle2, ClipboardCheck, Clock3, History, MonitorUp, RefreshCw, Settings2, ShieldAlert, Upload, XCircle } from 'lucide-react';
import { EmptyState, IconButton, PageHeader } from '../components/Common';

type ApprovalTab = 'pending' | 'history';

const STATUS_LABEL: Record<ApprovalFact['status'], string> = {
  pending: '待处理',
  approved: '已批准',
  rejected: '已拒绝',
  expired: '已过期',
  interrupted: '已中断',
};

function formatTime(timestamp: number) {
  if (!timestamp) return '-';
  return new Date(timestamp * 1000).toLocaleString('zh-CN', { hour12: false });
}

function FactIcon({ status }: { status: ApprovalFact['status'] }) {
  if (status === 'approved') return <CheckCircle2 size={18} />;
  if (status === 'rejected') return <XCircle size={18} />;
  return <ShieldAlert size={18} />;
}

function ApprovalTabs({ activeTab, pendingCount, historyCount, onChange }: {
  activeTab: ApprovalTab;
  pendingCount: number;
  historyCount: number;
  onChange: (tab: ApprovalTab) => void;
}) {
  const tabs: { id: ApprovalTab; label: string; count: number; icon: typeof ClipboardCheck }[] = [
    { id: 'pending', label: '待处理', count: pendingCount, icon: ClipboardCheck },
    { id: 'history', label: '最近记录', count: historyCount, icon: History },
  ];

  function handleKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
    event.preventDefault();
    const currentIndex = tabs.findIndex(tab => tab.id === activeTab);
    const nextIndex = event.key === 'Home'
      ? 0
      : event.key === 'End'
        ? tabs.length - 1
        : (currentIndex + (event.key === 'ArrowRight' ? 1 : -1) + tabs.length) % tabs.length;
    onChange(tabs[nextIndex].id);
    window.setTimeout(() => document.getElementById(`approval-tab-${tabs[nextIndex].id}`)?.focus(), 0);
  }

  return (
    <div className="approval-tabs" role="tablist" aria-label="审批视图" onKeyDown={handleKeyDown}>
      {tabs.map(({ id, label, count, icon: Icon }) => (
        <button
          type="button"
          key={id}
          id={`approval-tab-${id}`}
          role="tab"
          aria-selected={activeTab === id}
          aria-controls={`approval-panel-${id}`}
          tabIndex={activeTab === id ? 0 : -1}
          className={`approval-tab${activeTab === id ? ' active' : ''}`}
          onClick={() => onChange(id)}
        >
          <Icon size={15} aria-hidden="true" />
          <span>{label}</span>
          <span className={`approval-tab-count${id === 'pending' && count > 0 ? ' has-pending' : ''}`} aria-label={`${count} 条`}>
            {count}
          </span>
        </button>
      ))}
    </div>
  );
}

function PendingApprovals({ approvals, onRespond }: { approvals: ApprovalItem[]; onRespond: (id: string, approved: boolean) => void }) {
  if (approvals.length === 0) {
    return <EmptyState icon={ClipboardCheck} title="没有待处理请求" text="新的敏感操作请求会显示在这里。" />;
  }
  return (
    <div className="approval-list">
      {approvals.map(item => (
        <div className="approval-item" key={item.id}>
          <div className={`approval-icon ${item.request_type}`}>
            {item.request_type === 'remote_connect' ? <MonitorUp size={18} /> : <Upload size={18} />}
          </div>
          <div className="info"><div className="title">{item.title}</div><div className="desc">{item.description}</div></div>
          <span className="timer"><Clock3 size={14} />剩余 {item.remaining_seconds ?? item.timeout_seconds ?? 30} 秒</span>
          <div className="actions">
            <button type="button" className="btn" onClick={() => onRespond(item.id, false)}>拒绝</button>
            <button type="button" className="btn btn-primary" onClick={() => onRespond(item.id, true)}>允许</button>
          </div>
        </div>
      ))}
    </div>
  );
}

function ApprovalHistory({ history }: { history: ApprovalFact[] }) {
  if (history.length === 0) {
    return <EmptyState icon={History} title="暂无审批记录" text="审批决定和自动授权记录会显示在这里。" />;
  }
  return (
    <div className="approval-list">
      {history.slice(0, 100).map(item => (
        <div className="approval-item approval-history-item" key={item.id}>
          <div className={`approval-icon approval-status-${item.status}`}><FactIcon status={item.status} /></div>
          <div className="info"><div className="title">{item.title}</div><div className="desc">{item.description}</div></div>
          <span className={`approval-status approval-status-${item.status}`}>{STATUS_LABEL[item.status]}</span>
          <time className="timer" dateTime={new Date(item.created_at_unix * 1000).toISOString()}>{formatTime(item.created_at_unix)}</time>
        </div>
      ))}
    </div>
  );
}

export function ApprovalsPage({ approvals, history, independentMode = false, onRefresh, onRespond, onOpenSettings }: {
  approvals: ApprovalItem[];
  history: ApprovalFact[];
  independentMode?: boolean;
  onRefresh: () => void;
  onRespond: (id: string, approved: boolean) => void;
  onOpenSettings: () => void;
}) {
  const [activeTab, setActiveTab] = useState<ApprovalTab>('pending');

  return (
    <>
      <PageHeader title="审批" description={independentMode ? '处理本机 HiMind Agent 能力的确认请求；独立模式不依赖 Dashboard。' : '处理需要确认的敏感操作请求。'} actions={<div className="page-header-actions"><button type="button" className="btn" onClick={onOpenSettings}><Settings2 size={15} />审批策略</button><IconButton icon={RefreshCw} label="刷新审批" onClick={onRefresh} /></div>} />
      {independentMode ? <div className="security-note compact approval-independent-note"><ShieldAlert size={16} /><span>当前为独立模式：本机审批队列、历史和提醒仍有效。未经过 HiMind Agent 能力层、由 DSH 或外部 AI 工具直接执行的操作不受这里的策略拦截。</span></div> : null}
      <div className="card approval-workspace">
        <div className="approval-workspace-header">
          <ApprovalTabs activeTab={activeTab} pendingCount={approvals.length} historyCount={history.length} onChange={setActiveTab} />
          <span className="approval-workspace-meta">{activeTab === 'pending' ? '需要你决定的请求' : '只读记录'}</span>
        </div>
        <div id="approval-panel-pending" role="tabpanel" aria-labelledby="approval-tab-pending" hidden={activeTab !== 'pending'} className="card-body approval-tab-panel">
          <PendingApprovals approvals={approvals} onRespond={onRespond} />
        </div>
        <div id="approval-panel-history" role="tabpanel" aria-labelledby="approval-tab-history" hidden={activeTab !== 'history'} className="card-body approval-tab-panel">
          <ApprovalHistory history={history} />
        </div>
      </div>
    </>
  );
}
