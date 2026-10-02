import type { LogItem } from '../services/agentApi';
import { useEffect, useState } from 'react';
import { Download, FileText } from 'lucide-react';
import { EmptyState, IconButton, PageHeader } from '../components/Common';

/**
 * 运行时日志在「数据与诊断」里是一个页签，嵌进设置窗口时不能再自己画一遍页头，
 * 否则会出现「设置 → 数据与诊断」两个标题。列表本身仍受一个固定高度约束，
 * 不然几万行日志会把整个设置页拉长。
 */
export function LogsPage({ logs, onExport, embedded = false }: { logs: LogItem[]; onExport: () => void; embedded?: boolean }) {
  const actions = <IconButton icon={Download} label="导出诊断包" onClick={onExport} />;
  // 日志每 5 秒自动刷新，这里只回报“更新于”，不再给手动刷新按钮。
  const [updatedAt, setUpdatedAt] = useState(() => new Date());
  useEffect(() => { setUpdatedAt(new Date()); }, [logs]);
  const stamp = updatedAt.toLocaleTimeString('zh-CN', { hour12: false });
  if (logs.length === 0) {
    return <div className={embedded ? 'logs-page embedded' : 'logs-page'}>
      {embedded ? null : <PageHeader title="运行日志" description="查看最近的运行事件与错误。" actions={actions} />}
      <div className="card logs-card">
        {embedded ? <div className="card-header"><span>运行日志</span>{actions}</div> : null}
        <div className="card-body"><EmptyState icon={FileText} title="暂无日志记录" text="新的运行事件会显示在这里。" /></div>
      </div>
    </div>;
  }
  const visibleLogs = [...logs].reverse();
  return (
    <div className={embedded ? 'logs-page embedded' : 'logs-page'}>
      {embedded ? null : <PageHeader title="运行日志" description="查看最近的运行事件与错误。" actions={actions} />}
      <div className="card logs-card">
        <div className="card-header">
          <span>{embedded ? '运行日志' : '最近日志'}</span>
          <span className="card-header-actions"><span className="section-meta">{visibleLogs.length} 条 · 更新于 {stamp}</span>{embedded ? actions : null}</span>
        </div>
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
