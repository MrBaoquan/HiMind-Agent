import { useCallback, useEffect, useMemo, useState } from 'react';
import { AlertCircle, CheckCircle2, Clock3, ExternalLink, ListChecks, LoaderCircle, RefreshCw, Search, XCircle } from 'lucide-react';
import { EmptyState, PageHeader } from '../components/Common';
import type { AgentTaskHistoryItem, CurrentTaskStatus } from '../services/agentApi';
import {
  TASK_ACTIVE_STATUSES,
  TASK_ATTENTION_STATUSES,
  TASK_COMPLETED_STATUSES,
  formatAbsoluteTime,
  formatDuration,
  formatRelativeTime,
  taskStatusLabel,
  taskStatusTone,
  taskTypeLabel,
} from './taskView';

type TaskFilter = 'all' | 'running' | 'completed' | 'attention';

type TaskCenterPageProps = {
  currentTask: CurrentTaskStatus | null;
  dashboardEnabled: boolean;
  onLoadTaskHistory: () => Promise<AgentTaskHistoryItem[]>;
  onOpenDashboard: () => void;
};

const ACTIVE_STATUSES = TASK_ACTIVE_STATUSES;
const COMPLETED_STATUSES = TASK_COMPLETED_STATUSES;
const ATTENTION_STATUSES = TASK_ATTENTION_STATUSES;

const FILTERS: { id: TaskFilter; label: string }[] = [
  { id: 'all', label: '全部' },
  { id: 'running', label: '进行中' },
  { id: 'completed', label: '已完成' },
  { id: 'attention', label: '需处理' },
];

export function TaskCenterPage({ currentTask, dashboardEnabled, onLoadTaskHistory, onOpenDashboard }: TaskCenterPageProps) {
  const [items, setItems] = useState<AgentTaskHistoryItem[]>([]);
  const [filter, setFilter] = useState<TaskFilter>('all');
  const [query, setQuery] = useState('');
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');

  const load = useCallback(async (silent = false) => {
    if (!silent) setLoading(true);
    try {
      setItems(await onLoadTaskHistory());
      setError('');
    } catch (reason) {
      setError(typeof reason === 'string' ? reason : '暂时无法读取任务记录，请稍后重试。');
    } finally {
      if (!silent) setLoading(false);
    }
  }, [onLoadTaskHistory]);

  useEffect(() => {
    if (!dashboardEnabled) {
      setLoading(false);
      return;
    }
    void load();
    const timer = window.setInterval(() => {
      if (document.visibilityState !== 'hidden') void load(true);
    }, 5000);
    return () => window.clearInterval(timer);
  }, [dashboardEnabled, load]);

  const active = items.filter(item => ACTIVE_STATUSES.has(item.status));
  const completed = items.filter(item => COMPLETED_STATUSES.has(item.status));
  const attention = items.filter(item => ATTENTION_STATUSES.has(item.status));
  const currentItem = currentTask ? items.find(item => item.id === currentTask.task_id) : active[0];
  const filteredItems = useMemo(() => {
    const normalizedQuery = query.trim().toLowerCase();
    return items.filter(item => {
      const matchesFilter = filter === 'all'
        || (filter === 'running' && ACTIVE_STATUSES.has(item.status))
        || (filter === 'completed' && COMPLETED_STATUSES.has(item.status))
        || (filter === 'attention' && ATTENTION_STATUSES.has(item.status));
      if (!matchesFilter) return false;
      if (!normalizedQuery) return true;
      return [item.id, taskTypeLabel(item.task_type), item.detail, item.error]
        .filter(Boolean)
        .some(value => String(value).toLowerCase().includes(normalizedQuery));
    });
  }, [filter, items, query]);

  if (!dashboardEnabled) {
    return (
      <div className="task-center-page">
        <PageHeader title="任务中心" description="查看 Agent 正在执行和最近完成的任务。" />
        <div className="task-center-boundary" role="status">
          <ListChecks size={18} />
          <div><strong>当前处于独立模式</strong><span>任务中心需要连接 HiMind 工作台后使用。本机 AI、技能和插件仍可正常运行。</span></div>
        </div>
      </div>
    );
  }

  return (
    <div className="task-center-page">
      <PageHeader
        title="任务中心"
        description="查看正在执行和最近完成的任务，结果会自动同步。"
        actions={<><button type="button" className="btn" onClick={() => void load()} disabled={loading}><RefreshCw size={15} className={loading ? 'spin' : ''} />刷新</button><button type="button" className="btn btn-primary" onClick={onOpenDashboard}><ExternalLink size={15} />打开工作台</button></>}
      />

      <section className="task-center-summary" aria-label="任务概览">
        <SummaryItem label="当前执行" value={active.length} tone="running" />
        <SummaryItem label="需处理" value={attention.length} tone={attention.length ? 'attention' : 'neutral'} />
        <SummaryItem label="已完成" value={completed.length} tone="success" />
        <SummaryItem label="记录总数" value={items.length} tone="neutral" />
      </section>

      {active.length > 0 ? (
        <section className="task-center-current" aria-labelledby="task-center-current-title">
          <div className="task-center-section-heading"><div><span className="task-center-section-kicker"><Clock3 size={14} />实时状态</span><h3 id="task-center-current-title">正在执行</h3></div><span>{active.length} 项</span></div>
          <div className="task-center-current-list">{active.map(item => <TaskRow key={item.id} item={item} featured={item.id === currentItem?.id} />)}</div>
        </section>
      ) : null}

      <section className="task-center-history" aria-labelledby="task-center-history-title">
        <div className="task-center-section-heading"><div><span className="task-center-section-kicker"><CheckCircle2 size={14} />可回看</span><h3 id="task-center-history-title">任务记录</h3></div><span>{filteredItems.length} / {items.length}</span></div>
        <div className="task-center-toolbar">
          <div className="task-center-filters" role="tablist" aria-label="任务状态筛选">
            {FILTERS.map(option => {
              const count = option.id === 'all' ? items.length : option.id === 'running' ? active.length : option.id === 'completed' ? completed.length : attention.length;
              return <button type="button" role="tab" aria-selected={filter === option.id} className={filter === option.id ? 'active' : ''} key={option.id} onClick={() => setFilter(option.id)}><span>{option.label}</span><small>{count}</small></button>;
            })}
          </div>
          <label className="task-center-search"><Search size={15} aria-hidden="true" /><span className="sr-only">搜索任务</span><input value={query} onChange={event => setQuery(event.target.value)} placeholder="搜索任务名称、编号或结果" /></label>
        </div>
        {error ? <div className="task-center-alert" role="alert"><AlertCircle size={16} /><span>{error}</span><button type="button" className="btn" onClick={() => void load()}>重新读取</button></div> : null}
        {loading && items.length === 0 ? <div className="task-center-loading"><LoaderCircle size={18} className="spin" />正在读取任务记录</div> : filteredItems.length > 0 ? <div className="task-center-list">{filteredItems.map(item => <TaskRow key={item.id} item={item} />)}</div> : <div className="task-center-empty"><EmptyState icon={filter === 'attention' ? XCircle : ListChecks} title={items.length === 0 ? '还没有任务记录' : '没有匹配的任务'} text={items.length === 0 ? '从 HiMind 工作台发起任务后，进度和结果会自动显示在这里。' : '可以换一个状态或搜索关键词。'} />{items.length === 0 ? <button type="button" className="btn btn-primary" onClick={onOpenDashboard}><ExternalLink size={15} />打开工作台发起任务</button> : null}</div>}
      </section>
    </div>
  );
}

function SummaryItem({ label, value, tone }: { label: string; value: number; tone: 'running' | 'attention' | 'success' | 'neutral' }) {
  return <div className={`task-center-summary-item ${tone}`}><span>{label}</span><strong>{value}</strong></div>;
}

function TaskRow({ item, featured = false }: { item: AgentTaskHistoryItem; featured?: boolean }) {
  const active = ACTIVE_STATUSES.has(item.status);
  const tone = taskStatusTone(item.status);
  const progress = Math.max(0, Math.min(100, item.progress || 0));
  const message = item.error || item.detail;
  return (
    <article className={`task-center-row${featured ? ' featured' : ''}`}>
      <div className={`task-center-status-dot ${tone}`} aria-hidden="true" />
      <div className="task-center-row-main">
        <div className="task-center-row-title"><strong>{taskTypeLabel(item.task_type)}</strong><span className={`task-center-status ${tone}`}>{taskStatusLabel(item.status)}</span></div>
        <div className="task-center-row-meta"><code>{item.id}</code><time title={formatAbsoluteTime(item.finished_at || item.updated_at || item.created_at)}>{formatRelativeTime(item.finished_at || item.updated_at || item.created_at)}</time>{formatDuration(item.started_at, item.finished_at || item.updated_at) ? <span>耗时 {formatDuration(item.started_at, item.finished_at || item.updated_at)}</span> : null}</div>
        {message ? <p className={item.error ? 'error' : ''}>{message}</p> : null}
        {active ? <div className="task-center-progress" role="progressbar" aria-label={`${taskTypeLabel(item.task_type)}进度`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={progress}><span style={{ width: `${progress}%` }} /></div> : null}
      </div>
      {active ? <strong className="task-center-progress-label">{progress}%</strong> : item.status === 'completed' ? <CheckCircle2 className="task-center-result-icon success" size={18} aria-label="已完成" /> : item.status === 'failed' ? <AlertCircle className="task-center-result-icon danger" size={18} aria-label="失败" /> : <Clock3 className="task-center-result-icon neutral" size={18} aria-label="已取消" />}
    </article>
  );
}
