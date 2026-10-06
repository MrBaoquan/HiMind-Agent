import { useMemo } from 'react';
import { Network, RefreshCw, TriangleAlert } from 'lucide-react';
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

/**
 * 用量（ADR 0113）。只做本机网关这一条口径：读数、按日趋势、按工具构成。
 * 平台口径留在工作台，不在 Agent 重复展示。
 */
export function LocalUsagePanel({ overview, gateway, range, busy, onRangeChange, onRefresh, onBindGateway, bindBusy }: LocalUsagePanelProps) {
  const groups = useMemo(() => overview?.breakdowns.client ?? [], [overview]);
  const totalTokens = groups.reduce((sum, group) => sum + group.tokens, 0);
  const gatewayClients = gateway?.gateway_clients ?? [];

  return (
    <section className="ai-usage-panel" aria-label="本机用量">
      <div className="ai-usage-head">
        <div className="ai-usage-title">
          <Network size={16} aria-hidden="true" />
          <strong>用量</strong>
          <span className="ai-usage-scope">本机网关</span>
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
          <span>还没有工具走本机网关</span>
          <button type="button" className="btn" style={{ marginLeft: 'auto' }} disabled={bindBusy} onClick={onBindGateway}>
            {bindBusy ? <BusyIndicator size={14} /> : null}去配置
          </button>
        </div>
      );
    }
    const direct = gateway?.direct_clients ?? [];
    return (
      <>
        <div className="ai-usage-kpis">
          <UsageKpi label="调用" value={formatCount(overview.requests)} meta={`${gatewayClients.length} 个工具`} />
          <UsageKpi label="Token" value={formatTokens(overview.input_tokens + overview.output_tokens)} meta={`输入 ${formatTokens(overview.input_tokens)} · 输出 ${formatTokens(overview.output_tokens)}`} />
          {overview.usage_unreported > 0
            ? <UsageKpi label="未上报" value={formatCount(overview.usage_unreported)} meta="只计入次数" tone="warning" />
            : null}
        </div>
        {overview.has_trend ? <TrendChart overview={overview} /> : <div className="ai-usage-trend-note">今日只有单日数据，切换近 7 天看趋势。</div>}
        <div className="ai-usage-breakdown">
          <div className="ai-usage-trend-head">
            <span>按工具</span>
          </div>
          {groups.length === 0 ? (
            <div className="ai-usage-trend-note">该区间内没有数据。</div>
          ) : (
            <table className="ai-usage-table">
              <thead>
                <tr>
                  <th scope="col">工具</th>
                  <th scope="col">调用</th>
                  <th scope="col">Token</th>
                  <th scope="col">占比</th>
                </tr>
              </thead>
              <tbody>
                {groups.slice(0, 10).map(row => {
                  const share = totalTokens > 0 ? row.tokens / totalTokens : 0;
                  return (
                    <tr key={row.key || row.label}>
                      <td title={row.label}>{row.label}</td>
                      <td>{formatCount(row.requests)}</td>
                      <td>{formatTokens(row.tokens)}</td>
                      <td>
                        <span className="ai-usage-share">
                          <span className="ai-usage-share-track" aria-hidden="true"><i style={{ width: `${Math.min(100, share * 100)}%` }} /></span>
                          {formatPercent(share)}
                        </span>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          )}
        </div>
        <p
          className="ai-usage-footnote"
          title={[
            '只统计经过本机网关的调用，不含金额（费用以服务商控制台为准）',
            direct.length ? `直连未计入：${direct.join('、')}` : '',
            overview.usage_unreported ? `${overview.usage_unreported} 次调用上游未返回用量，只计入了次数` : '',
          ].filter(Boolean).join('；')}
        >
          仅统计本机网关调用 · 不含费用{direct.length ? ` · ${direct.length} 个直连工具未计入` : ''}
        </p>
      </>
    );
  }
}

function UsageKpi({ label, value, meta, tone }: { label: string; value: string; meta: string; tone?: 'warning' }) {
  return (
    <div className={`ai-usage-kpi${tone ? ` ${tone}` : ''}`}>
      <span className="ai-usage-kpi-label">{label}</span>
      <strong>{value}</strong>
      <span className="ai-usage-kpi-meta">{meta}</span>
    </div>
  );
}

function TrendChart({ overview }: { overview: LocalUsageOverview }) {
  const values = overview.daily_tokens;
  const max = values.reduce((current, value) => Math.max(current, value), 0);
  const peak = max > 0 ? max : 1;
  // 高度用像素而不是百分比：图区高度是固定的（96px），像素值不受父级
  // 高度解析规则影响，避免出现「同一天数据在别的样式环境里变成 2px」。
  const plotHeight = 96;
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
