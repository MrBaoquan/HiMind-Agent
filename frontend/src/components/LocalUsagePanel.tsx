import { useMemo, useState } from 'react';
import { ChevronDown, ChevronUp, Network, RefreshCw } from 'lucide-react';
import { BusyIndicator } from './BusyIndicator';
import { IconButton } from './Common';
import type { AiUsageRange, InferenceGatewayStatus, LocalUsageGroup, LocalUsageOverview } from '../services/agentApi';

type LocalUsagePanelProps = {
  overview: LocalUsageOverview | null;
  gateway: InferenceGatewayStatus | null;
  range: AiUsageRange;
  busy: boolean;
  onRangeChange: (range: AiUsageRange) => void;
  onRefresh: () => void;
  onBindGateway: () => void;
  bindBusy: boolean;
};

const RANGE_OPTIONS: Array<{ key: AiUsageRange; label: string }> = [
  { key: 'today', label: '今日' },
  { key: '7d', label: '近 7 天' },
  { key: '30d', label: '近 30 天' },
];

/** 横条最多列几行。超过 5 行时后面的工具占比都很小，读图不如读数字。 */
const BREAKDOWN_ROWS = 5;

/**
 * 用量（ADR 0113 §4 的形态）。
 *
 * 固定三块：一行读数、按日趋势、按工具构成。不提供维度切换——网关口径唯一
 * 回答得了、别处回答不了的问题是「哪个工具在用」，模型与服务维度在网关状态
 * 面板里已有。失败明细与能力观测都不常驻：前者是排查时才需要的，后者是运维
 * 事实，已移到「AI 连接 → 模型网关」。
 */
export function LocalUsagePanel({ overview, gateway, range, busy, onRangeChange, onRefresh, onBindGateway, bindBusy }: LocalUsagePanelProps) {
  const [failuresOpen, setFailuresOpen] = useState(false);
  // 构成固定按工具：这是 ADR 0113 收敛时定下的口径，不随维度切换。
  const groups = useMemo(() => overview?.breakdowns.client ?? [], [overview]);
  const totalTokens = groups.reduce((sum, group) => sum + group.tokens, 0);
  const gatewayClients = gateway?.gateway_clients ?? [];
  const recentFailures = overview?.recent_failures ?? [];

  return (
    <section className="ai-usage-panel" aria-label="本机用量">
      <div className="ai-usage-head">
        <div className="ai-usage-title">
          <Network size={16} aria-hidden="true" />
          <strong>用量</strong>
          <span className="ai-usage-scope">本机推理网关</span>
        </div>
        <div className="ai-usage-tools">
          <div className="ai-usage-segmented" role="group" aria-label="本机用量统计范围">
            {RANGE_OPTIONS.map(option => (
              <button
                key={option.key}
                type="button"
                className={option.key === range ? 'active' : undefined}
                aria-pressed={option.key === range}
                onClick={() => onRangeChange(option.key)}
              >
                {option.label}
              </button>
            ))}
          </div>
          <IconButton icon={RefreshCw} label="刷新本机用量" onClick={onRefresh} disabled={busy} />
        </div>
      </div>
      {renderBody()}
    </section>
  );

  function renderBody() {
    if (gateway && !gateway.running) {
      return (
        <div className="ai-usage-notice neutral">
          <span>本机推理网关未启动，本机用量不可用。</span>
        </div>
      );
    }
    if (!overview) {
      return <div className="ai-usage-loading">{busy ? <><BusyIndicator size={14} />正在读取本机用量</> : <span>尚未读取本机用量</span>}</div>;
    }
    if (gatewayClients.length === 0) {
      return (
        <div className="ai-usage-notice neutral">
          <span>还没有工具走本机推理网关</span>
          <button type="button" className="btn" style={{ marginLeft: 'auto' }} disabled={bindBusy} onClick={onBindGateway}>
            {bindBusy ? <BusyIndicator size={14} /> : null}去配置
          </button>
        </div>
      );
    }
    const direct = gateway?.direct_clients ?? [];
    return (
      <>
        <p className="ai-usage-readout">
          <span>调用 <strong>{formatCount(overview.requests)}</strong></span>
          <span>Token <strong>{formatTokens(overview.input_tokens + overview.output_tokens)}</strong><small>输入 {formatTokens(overview.input_tokens)} · 输出 {formatTokens(overview.output_tokens)}</small></span>
          <span className={overview.failures > 0 ? 'warning' : undefined}>失败 <strong>{formatPercent(overview.failure_rate)}</strong><small>{formatCount(overview.failures)} 次</small></span>
          <span>P95 <strong>{formatCount(overview.p95_latency_ms)} ms</strong><small>平均 {formatCount(overview.avg_latency_ms)} ms</small></span>
          {overview.usage_unreported > 0
            ? <span className="warning">未上报 <strong>{formatCount(overview.usage_unreported)}</strong><small>只计次数</small></span>
            : null}
        </p>
        {overview.has_trend ? <TrendChart overview={overview} /> : <div className="ai-usage-trend-note">今日只有单日数据，切换近 7 天看趋势。</div>}
        <Breakdown groups={groups} totalTokens={totalTokens} />
        <FailureSummary
          failures={recentFailures}
          open={failuresOpen}
          onToggle={() => setFailuresOpen(current => !current)}
        />
        <p
          className="ai-usage-footnote"
          title={[
            '只统计经过本机推理网关的调用；费用以服务商控制台为准',
            direct.length ? `直连未计入：${direct.join('、')}` : '',
            overview.usage_unreported ? `${overview.usage_unreported} 次调用上游未返回用量，只计入了次数` : '',
          ].filter(Boolean).join('；')}
        >
          仅统计本机推理网关调用 · 不含费用{direct.length ? ` · ${direct.length} 个直连工具未计入` : ''}
        </p>
      </>
    );
  }
}

function TrendChart({ overview }: { overview: LocalUsageOverview }) {
  const values = overview.daily_tokens;
  const max = values.reduce((current, value) => Math.max(current, value), 0);
  const peak = max > 0 ? max : 1;
  // 高度用像素而不是百分比：图区高度是固定的，像素值不受父级高度解析规则影响，
  // 避免出现「同一天数据在别的样式环境里变成 2px」。
  const plotHeight = 140;
  return (
    <div className="ai-usage-trend">
      <div className="ai-usage-trend-head"><span>趋势</span><span>按日 Token</span></div>
      <div className="ai-usage-chart">
        <div className="ai-usage-chart-scale">
          <span>{formatTokens(max)}</span>
          <span>0</span>
        </div>
        <div className="ai-usage-bars" role="img" aria-label={`按日 Token 趋势，峰值 ${formatTokens(max)}`}>
          {values.map((value, index) => (
            <span
              key={overview.labels[index] ?? index}
              // 类名必须带前缀：`empty` 是应用级空态类（min-height:180px），
              // 裸用它会把柱子撑成一块 180px 高的灰块（实测过）。
              className={`ai-usage-bar${value <= 0 ? ' ai-usage-bar-empty' : ''}`}
              style={{ height: `${value <= 0 ? 2 : Math.max(2, Math.round((value / peak) * plotHeight))}px` }}
              title={`${overview.labels[index] ?? ''} ${formatTokens(value)}`}
            />
          ))}
        </div>
        <div className="ai-usage-chart-axis">
          <span>{formatDay(overview.labels[0] ?? '')}</span>
          <span>{formatDay(overview.labels[overview.labels.length - 1] ?? '')}</span>
        </div>
      </div>
    </div>
  );
}

/**
 * 按工具构成。用横条而不是表格：这里只有一个维度，表格的「调用/失败/Token/占比」
 * 四列里只有占比值得扫一眼，其余数字放悬停。
 */
function Breakdown({ groups, totalTokens }: { groups: LocalUsageGroup[]; totalTokens: number }) {
  const rows = groups.slice(0, BREAKDOWN_ROWS);
  return (
    <div className="ai-usage-breakdown">
      <div className="ai-usage-trend-head"><span>按工具</span>{rows.length ? null : <span>该区间内没有数据</span>}</div>
      {rows.length ? (
        <ul className="ai-usage-legend">
          {rows.map(row => {
            const share = totalTokens > 0 ? row.tokens / totalTokens : 0;
            return (
              <li key={row.key || row.label}>
                <span className="ai-usage-legend-label" title={row.label}>{row.label}</span>
                <span className="ai-usage-legend-track" aria-hidden="true"><i style={{ width: `${Math.min(100, share * 100)}%` }} /></span>
                <span className="ai-usage-legend-share">{formatPercent(share)}</span>
                <span
                  className="ai-usage-legend-meta"
                  title={[
                    `${formatCount(row.requests)} 次调用`,
                    row.failures > 0 ? `${formatCount(row.failures)} 次失败（${formatPercent(row.failure_rate)}）` : '没有失败',
                    `平均延迟 ${formatCount(row.avg_latency_ms)} ms`,
                  ].join(' · ')}
                >
                  {formatTokens(row.tokens)}
                </span>
              </li>
            );
          })}
        </ul>
      ) : null}
    </div>
  );
}

/**
 * 失败摘要：平时只占一行，展开才出明细。
 *
 * 失败表的价值在排查时，不在平时扫一眼时——常驻 10 行等于为偶发事件付出永久空间。
 */
function FailureSummary({ failures, open, onToggle }: { failures: LocalUsageOverview['recent_failures']; open: boolean; onToggle: () => void }) {
  const latest = failures[0];
  return (
    <div className="ai-usage-failures">
      <button
        type="button"
        className="ai-usage-failure-summary"
        aria-expanded={open}
        onClick={onToggle}
        disabled={failures.length === 0}
      >
        <span className="ai-usage-failure-text">
          {failures.length === 0
            ? '该区间内没有失败调用'
            : latest
              ? `最近失败：${latest.client} · ${latest.status || 'failure'} · ${formatFailureTime(latest.occurred_at)}`
              : '查看失败明细'}
        </span>
        {failures.length ? (
          <span className="ai-usage-failure-count">
            {open ? <ChevronUp size={13} /> : <ChevronDown size={13} />}
            {open ? '收起' : `${failures.length} 条`}
          </span>
        ) : null}
      </button>
      {open && failures.length ? (
        <table className="ai-usage-table">
          <thead>
            <tr>
              <th scope="col">时间</th>
              <th scope="col">工具</th>
              <th scope="col">模型</th>
              <th scope="col">状态</th>
              <th scope="col">请求 ID</th>
              <th scope="col">延迟</th>
            </tr>
          </thead>
          <tbody>
            {failures.slice(0, 10).map(failure => (
              <tr key={`${failure.request_id}-${failure.occurred_at}`}>
                <td>{formatFailureTime(failure.occurred_at)}</td>
                <td title={failure.client}>{failure.client}</td>
                <td title={failure.model}>{failure.model}</td>
                <td>{failure.status || 'failure'}</td>
                <td title={failure.request_id}><code>{failure.request_id || '—'}</code></td>
                <td>{formatCount(failure.latency_ms)} ms</td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : null}
    </div>
  );
}

function formatCount(value: number): string {
  return Math.round(value).toLocaleString('zh-CN');
}

function formatTokens(value: number): string {
  const abs = Math.abs(value);
  if (abs >= 1_000_000_000) return `${(value / 1_000_000_000).toFixed(2)}B`;
  if (abs >= 1_000_000) return `${(value / 1_000_000).toFixed(2)}M`;
  if (abs >= 1_000) return `${(value / 1_000).toFixed(1)}K`;
  return formatCount(value);
}

function formatPercent(value: number): string {
  return `${(value * 100).toFixed(1)}%`;
}

function formatDay(value: string): string {
  return value.length >= 10 ? value.slice(5, 10) : value;
}

function formatFailureTime(value: string): string {
  if (value.length < 16) return value;
  return `${value.slice(5, 10).replace('-', '/')} ${value.slice(11, 16)}`;
}
