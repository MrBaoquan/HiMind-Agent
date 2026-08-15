import type { LogItem } from '../services/agentApi';
import { Download, FileText, RefreshCw } from 'lucide-react';
import { EmptyState, IconButton, PageHeader } from '../components/Common';

export function LogsPage({ logs, onRefresh, onExport }: { logs: LogItem[]; onRefresh: () => void; onExport: () => void }) {
  const actions = <><IconButton icon={Download} label="导出诊断包" onClick={onExport} /><IconButton icon={RefreshCw} label="刷新日志" onClick={onRefresh} /></>;
  if (logs.length === 0) {
    return <div className="logs-page"><PageHeader title="运行日志" description="查看当前 Agent 最近的运行事件与错误。" actions={actions} /><div className="card logs-card"><div className="card-body"><EmptyState icon={FileText} title="暂无日志记录" text="Agent 产生新的运行事件后会显示在这里。" /></div></div></div>;
  }
  const visibleLogs = [...logs].reverse();
  return (
    <div className="logs-page">
      <PageHeader title="运行日志" description="查看当前 Agent 最近的运行事件与错误。" actions={actions} />
      <div className="card logs-card">
        <div className="card-header"><span>最近日志</span><span className="section-meta">{visibleLogs.length} 条</span></div>
        <div className="card-body log-list" role="list" aria-label="运行日志列表" tabIndex={0}>
          {visibleLogs.map((item, index) => (
            <div className="log-entry" role="listitem" key={`${item.timestamp || item.time || ''}-${index}`}>
              <time className="time" dateTime={item.timestamp ? new Date(item.timestamp * 1000).toISOString() : undefined}>{formatLogTime(item)}</time>
              <span className={`level ${item.level || 'info'}`}>{(item.level || 'info').toUpperCase()}</span>
              <span className="msg">{item.message || ''}</span>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

function formatLogTime(item: LogItem) {
  if (!item.timestamp) return item.time || '--';
  return new Date(item.timestamp * 1000).toLocaleString('zh-CN', { hour12: false });
}
