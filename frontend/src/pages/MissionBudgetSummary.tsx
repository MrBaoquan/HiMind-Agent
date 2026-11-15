import type { MissionEvent, MissionView } from '../services/agentApi';

export function MissionBudgetSummary({mission, events, historyComplete = false, now}: {mission: MissionView; events: MissionEvent[]; historyComplete?: boolean; now: number}) {
  const budget = mission.budget;
  if (!budget) return <p className="muted">预算待计划生成</p>;
  // Sequence numbers are global across Missions; gaps alone are not lost rows.
  const rounds = historyComplete ? events.filter(event => event.event_type === 'task_dispatched').length : null;
  const confirmed = Date.parse(mission.confirmation?.confirmed_at || '');
  const minutes = Number.isFinite(confirmed) ? Math.max(0,Math.floor((now-confirmed)/60_000)) : null;
  const repairs = mission.tasks.filter(task => task.spec.repair_of).length;
  const tokens = mission.usage ? mission.usage.input_tokens + mission.usage.output_tokens : null;
  const remaining = (limit: number | null, used: number | null, unit = '') => limit == null ? '不限' : used == null
    ? `上限 ${limit.toLocaleString()}${unit} · 剩余未知` : `剩余 ${Math.max(0,limit-used).toLocaleString()}${unit} / 上限 ${limit.toLocaleString()}${unit}`;
  const cost = mission.cost_usage;
  const currency = budget.currency?.trim().toUpperCase();
  const measured = cost?.comparable && currency && Object.keys(cost.buckets).length === 1 ? cost.buckets[currency] : null;
  return <section className="mission-budget-summary" aria-label="所有预算限制"><h3>所有预算限制</h3><dl>
    <div><dt>派发轮次</dt><dd>{remaining(budget.max_rounds, rounds)}</dd></div>
    <div><dt>时限</dt><dd>{minutes !== null && minutes >= budget.max_minutes ? `时限已耗尽 · 剩余 0 分钟 / 上限 ${budget.max_minutes.toLocaleString()} 分钟` : remaining(budget.max_minutes, minutes,' 分钟')}</dd></div>
    <div><dt>返工次数</dt><dd>{remaining(budget.max_repair_attempts, repairs)}</dd></div>
    <div><dt>Token（按已报用量）</dt><dd>{remaining(budget.max_total_tokens, tokens)}{tokens !== null ? ` · 已报 ${tokens.toLocaleString()}` : ' · 用量未报告'}</dd></div>
    <div><dt>成本{currency ? `（${currency}）` : ''}</dt><dd>{budget.max_cost == null ? '不限' : remaining(budget.max_cost, measured ?? null)}{budget.max_cost != null && !currency ? ' · 币种未提供，无法比较' : ''}</dd></div>
  </dl><p>已报告成本：{cost && Object.keys(cost.buckets).length ? Object.entries(cost.buckets).map(([unit,value]) => `${value} ${unit}`).join('；') : '未知'}{cost?.unknown_runs ? `；${cost.unknown_runs} 次运行成本不完整` : ''}。不同币种不合计；未上报用量不补零。</p>
    <p>时限从确认至今按整分钟累计，达到上限即耗尽。<br />暂停、环境、预算和核验等待均计入时限。</p>
    {['completed','failed','canceled'].includes(mission.status) ? <p>任务已结束，仍显示确认至今的累计时间，并非结算运行耗时。</p> : null}
    <p>本机时钟仅供展示，最终资格由系统复核。</p></section>;
}
