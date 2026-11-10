import { Activity } from 'lucide-react';
import type { InferenceGatewayStatus } from '../services/agentApi';

type GatewayObservationsPanelProps = {
  observations: NonNullable<InferenceGatewayStatus['capability_observations']>;
};

/**
 * 能力观测（`inference-gateway-governance.md` §6）。
 *
 * 记录的是「兼容降级后上游仍然成功」这一事实：某家上游拒收 `tool_result` 图片或
 * `function_call` 历史时，网关会先降级再发送。它是**运维事实**，不是用量——
 * 放在用量面板里会让首页常驻一张运维表，因此归到「模型网关」页签。
 *
 * 观测 7 天过期、需累计 2 次成功回退才进入预降级，所以这里的条数通常是个位数。
 */
export function GatewayObservationsPanel({ observations }: GatewayObservationsPanelProps) {
  return (
    <section className="ai-client-section gateway-observations-section">
      <div className="ai-section-heading">
        <div>
          <h3>能力观测</h3>
          <span>上游拒绝富工具输入时，网关降级后仍成功的记录</span>
        </div>
        <span className="gateway-observations-count"><Activity size={13} />{observations.length} 项</span>
      </div>
      <p className="gateway-observations-note">
        这些能力在对接该上游时会自动降级，不需要手动配置；观测 7 天后过期，上游修好后会自动恢复完整能力。
      </p>
      <table className="ai-usage-table gateway-observations-table">
        <thead>
          <tr>
            <th scope="col">上游</th>
            <th scope="col">协议</th>
            <th scope="col">能力</th>
            <th scope="col">成功回退</th>
            <th scope="col">最近观测</th>
          </tr>
        </thead>
        <tbody>
          {observations.map(item => (
            <tr key={`${item.provider}-${item.protocol}-${item.feature}`}>
              <td title={item.provider}>{item.provider}</td>
              <td>{item.protocol}</td>
              <td>{item.feature}</td>
              <td>{item.successes.toLocaleString('zh-CN')}</td>
              <td>{formatObservedAt(item.last_seen)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}

function formatObservedAt(value: string): string {
  if (value.length < 16) return value || '—';
  return `${value.slice(5, 10).replace('-', '/')} ${value.slice(11, 16)}`;
}
