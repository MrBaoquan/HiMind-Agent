// 预检失败时给用户看的一句话：先说哪里不满足、该怎么处理，再补上系统返回的原始原因。
// 原始原因（`connector xxx health probe failed: ...`）是给排查用的，直接甩到界面上
// 既读不懂、也不说明该做什么；但排查时又需要它，所以放在最后而不是丢掉。
import type { WorkflowPreflight } from '../services/agentApi';

type Diagnostic = WorkflowPreflight['diagnostics'][number];

/** 已知阻塞码对应的中文说明；没覆盖到的码退回原始原因，不猜。 */
const BLOCKER_LEADS: Record<string, string> = {
  'connector.health_probe_failed': '连接器未通过健康检查',
  'connector.credential_missing': '缺少连接器凭据',
  'connector.credential_mismatch': '连接器凭据不匹配',
  'connector.credentials.store_unavailable': '凭据存储不可用',
  'connector.unavailable': '连接器不可用',
  'connector.inactive': '连接器已停用',
  'connector.capability_undeclared': '连接器未声明所需功能',
  'connector.dashboard_required': '需要先连接 AI 工作台账号',
  'capability.unavailable': '缺少必需的功能',
  'capability.input.invalid': '功能参数不合法',
  'skill.unavailable': '缺少必需的技能',
  'plugin.unavailable': '缺少必需的插件',
  'runtime.unavailable': '运行环境不可用',
  'runtime.dependency_unavailable': '运行环境依赖不可用',
  'runtime.network_isolation_missing': '运行环境缺少网络隔离',
  'runtime.auto_unavailable': '运行环境不可用',
  'runtime.auto_isolation_unavailable': '运行环境无法自动隔离',
  'tool.required_unavailable': '缺少必需的工具',
  'agent.version.too_old': 'Agent 版本过旧',
  'workflow.version.invalid': '工作流版本不受支持',
  'approval.required': '需要审批后才能运行',
};

export function blockerDiagnostics(report: WorkflowPreflight): Diagnostic[] {
  return (report.diagnostics || []).filter(item => item.severity === 'blocker');
}

export function blockerLead(code: string) {
  return BLOCKER_LEADS[code] || '';
}

/** 一行高度的结论，给标题旁边的小字用：中文结论优先，拿不到码才退回原始原因。 */
export function blockerHeadline(report: WorkflowPreflight) {
  const first = blockerDiagnostics(report)[0];
  return (blockerLead(first?.code || '') || first?.message || report.blockers?.[0] || '').trim();
}

/**
 * 阻塞项的汇总结论：同类合并计数。一次预检里「缺少必需的功能」可能有十几条，
 * 原样铺开就是同一句话说十几遍；按首次出现顺序合并后既看得懂，也不丢条数。
 */
export function blockerSummary(report: WorkflowPreflight) {
  const counted = new Map<string, number>();
  for (const item of blockerDiagnostics(report)) {
    const lead = (blockerLead(item.code) || item.message || '').trim();
    if (!lead) continue;
    counted.set(lead, (counted.get(lead) || 0) + 1);
  }
  return [...counted.entries()].map(([lead, count]) => (count > 1 ? `${lead}（${count} 项）` : lead)).join('；');
}

/**
 * 「哪里不满足 → 怎么办 → 原始原因」。
 * 处理建议来自后端 remediation，已经是中文，不在这里重写第二套文案。
 */
export function preflightNote(report: WorkflowPreflight, fallback = '运行条件未就绪'): string {
  const first = blockerDiagnostics(report)[0];
  const raw = (first?.message || report.blockers?.[0] || '').trim();
  const remedy = (first?.remediation || '').trim();
  const head = blockerLead(first?.code || '') || raw || fallback;
  const parts = [head];
  if (remedy && remedy !== head) parts.push(remedy);
  if (raw && raw !== head && raw !== remedy) parts.push(`原因：${raw}`);
  // 后端给的建议本身可能带句号（「…Plugin/Connector。」），拼起来会出现「。。」，
  // 统一去掉尾部的句号再拼接，一句话只有一个结尾。
  return `${parts.map(part => part.replace(/[。.]+$/, '')).join('。')}。`;
}
