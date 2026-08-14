import type { LogItem } from '../services/agentApi';
import { Download, FileText, RefreshCw } from 'lucide-react';
import { EmptyState, IconButton, PageHeader } from '../components/Common';

export function LogsPage({ logs, onRefresh, onExport }: { logs: LogItem[]; onRefresh: () => void; onExport: () => void }) {
  const actions = <><IconButton icon={Download} label="导出诊断包" onClick={onExport} /><IconButton icon={RefreshCw} label="刷新日志" onClick={onRefresh} /></>;
  if (logs.length === 0) {
    return <><PageHeader title="运行日志" description="查看当前 Agent 最近的运行事件与错误。" actions={actions} /><div className="card"><div className="card-body"><EmptyState icon={FileText} title="暂无日志记录" text="Agent 产生新的运行事件后会显示在这里。" /></div></div></>;
  }
  return (
    <>
      <PageHeader title="运行日志" description="查看当前 Agent 最近的运行事件与错误。" actions={actions} />
      <div className="card">
        <div className="card-header"><span>最近日志</span><span className="section-meta">最近 {Math.min(logs.length, 120)} 条</span></div>
        <div className="card-body log-list">
          {logs.slice(-120).reverse().map((item, index) => (
            <div className="log-entry" key={`${item.time || ''}-${index}`}>
              <span className="time">{item.time || ''}</span>
              <span className={`level ${item.level || 'info'}`}>{(item.level || 'info').toUpperCase()}</span>
              <span className="msg">{item.message || ''}</span>
            </div>
          ))}
        </div>
      </div>
    </>
  );
}
