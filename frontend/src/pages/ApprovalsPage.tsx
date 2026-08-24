import type { ApprovalItem } from '../services/agentApi';
import { ClipboardCheck, Clock3, MonitorUp, RefreshCw, Upload } from 'lucide-react';
import { EmptyState, IconButton, PageHeader } from '../components/Common';

export function ApprovalsPage({ approvals, onRefresh, onRespond }: {
  approvals: ApprovalItem[];
  onRefresh: () => void;
  onRespond: (id: string, approved: boolean) => void;
}) {
  return (
    <>
      <PageHeader title="审批" description="处理需要确认的敏感操作请求。" actions={<IconButton icon={RefreshCw} label="刷新审批" onClick={onRefresh} />} />
      <div className="card">
        <div className="card-header"><span>待处理请求</span><span className="section-count">{approvals.length}</span></div>
        <div className="card-body">
          {approvals.length === 0 ? <EmptyState icon={ClipboardCheck} title="没有待处理请求" text="新的敏感操作请求会显示在这里。" /> : <div className="approval-list">{approvals.map(item => (
            <div className="approval-item" key={item.id}>
              <div className={`approval-icon ${item.request_type}`}>{item.request_type === 'remote_connect' ? <MonitorUp size={18} /> : <Upload size={18} />}</div>
              <div className="info"><div className="title">{item.title}</div><div className="desc">{item.description}</div></div>
              <span className="timer"><Clock3 size={14} />剩余 {item.remaining_seconds ?? item.timeout_seconds ?? 30} 秒</span>
              <div className="actions">
                <button className="btn" onClick={() => onRespond(item.id, false)}>拒绝</button>
                <button className="btn btn-primary" onClick={() => onRespond(item.id, true)}>允许</button>
              </div>
            </div>
          ))}</div>}
        </div>
      </div>
    </>
  );
}
