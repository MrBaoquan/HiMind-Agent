import { useEffect, useMemo, useState } from 'react';
import { ArrowLeft, CircleAlert, CircleCheck, RefreshCw, ShieldCheck, Unplug } from 'lucide-react';
import { BusyIndicator } from '../components/BusyIndicator';
import { EmptyState, Pill } from '../components/Common';
import { ExtensionKindMark } from '../components/ExtensionKindMark';
import { extensionKindLabels, type ExtensionKind } from '../data/extensionKinds';
import type { CodexSkillStatusResponse, ExtensionDesiredItem, ExtensionDesiredState, PluginRegistry, WorkflowCenterItem } from '../services/agentApi';
import { pluginNeedsRepair } from './pluginHealth';

export type ManagedCapabilityKind = 'plugin' | 'skill' | 'workflow';

/// 面板既能按类型显示，也能在「我的能力 → 组织管理」里一次看全所有类型。
export type ManagedCapabilityScope = ManagedCapabilityKind | 'all';

type ManagedCapabilitiesPageProps = {
  assetKind: ManagedCapabilityScope;
  desired: ExtensionDesiredState | null;
  loading: boolean;
  error: string | null;
  registry: PluginRegistry | null;
  skillStatus: CodexSkillStatusResponse | null;
  /// 工作流的本机状态来自工作流中心，其余类型用各自的运行态。
  workflows?: WorkflowCenterItem[];
  /// 组织管理的插件不能停用和卸载，但运行异常时必须让用户能自助恢复，所以只开放「修复并重试」。
  onRepairPlugin?: (pluginId: string) => void;
};

type LocalInfo = {
  installed: boolean;
  enabled: boolean;
  version: string;
  state: string;
  /// 插件专有：熔断或未过期的失败记录会挡掉依赖它的技能，需要在详情里给出修复出口。
  needsRepair?: boolean;
  failure?: string;
};

export function ManagedCapabilitiesPanel({ assetKind, desired, loading, error, registry, skillStatus, workflows = [], onRepairPlugin }: ManagedCapabilitiesPageProps) {
  const scopeLabel = assetKind === 'all' ? '能力' : extensionKindLabels[assetKind];
  const items = useMemo(() => managedItems(desired, registry, skillStatus).filter(item => assetKind === 'all' || item.asset_kind === assetKind), [assetKind, desired, registry, skillStatus]);
  const [selectedKey, setSelectedKey] = useState('');
  const [detailOpen, setDetailOpen] = useState(false);
  useEffect(() => {
    if (!items.some(item => `${item.asset_kind}:${item.asset_key}` === selectedKey)) {
      setSelectedKey(items[0] ? `${items[0].asset_kind}:${items[0].asset_key}` : '');
    }
  }, [items, selectedKey]);
  const selected = items.find(item => `${item.asset_kind}:${item.asset_key}` === selectedKey) || items[0];
  const localFor = (item: ExtensionDesiredItem) => getLocalInfo(item, registry, skillStatus, workflows);
  // 策略读不到时（例如 HiMind 账号还没授权），这一页只剩本机实况：
  // 既不能拿本机版本冒充「期望版本」，也不能给条目盖上「已符合策略」的章。
  const policyReady = Boolean(desired);
  const attentionCount = policyReady
    ? items.filter(item => !isCompliant(item, localFor(item))).length
    : items.filter(item => localFor(item).needsRepair).length;
  const builtinCount = items.filter(item => policyLabel(item) === '系统内置').length;
  const requiredCount = items.filter(item => policyLabel(item) === '组织必装').length;
  const managedCount = items.filter(item => policyLabel(item) === '组织管理').length;

  if (loading && !desired) return <div className="page-loading"><BusyIndicator size={15} />正在读取组织管理的{scopeLabel}</div>;

  return (
    <div className="managed-page managed-panel">
      {error ? <div className="blocker"><CircleAlert size={18} /><div><strong>组织策略暂不可用</strong><span>{error}{items.length ? ' 下列条目按本机实际情况展示，授权后可核对策略。' : ''}</span></div></div> : null}
      {!error && !policyReady && !loading && items.length ? <div className="blocker neutral"><ShieldCheck size={18} /><div><strong>尚未读取组织策略</strong><span>授权 HiMind 账号后可核对期望版本与治理边界；当前为本机实际情况。</span></div></div> : null}
      {items.length ? <section className="managed-summary" aria-label="组织管理能力摘要">
        <div><span>系统内置</span><strong>{builtinCount}</strong></div>
        <div><span>组织必装</span><strong>{requiredCount}</strong></div>
        <div><span>组织管理</span><strong>{managedCount}</strong></div>
        <div><span>{policyReady ? '需处理' : '本机异常'}</span><strong className={attentionCount ? 'warning-text' : ''}>{attentionCount}</strong></div>
        <details className="managed-summary-note"><summary><ShieldCheck size={16} /><span>策略信息</span></summary><code>{desired?.generation || '尚未获取策略版本'}</code></details>
      </section> : null}

      {!items.length ? (
        <div className="managed-empty"><EmptyState icon={Unplug} title={`当前没有组织管理的${scopeLabel}`} text={error ? '连接 HiMind 账号后重试，位置在左下角账号菜单。' : '系统内置和组织管理的条目会显示在这里。'} /></div>
      ) : (
        <section className={`managed-workspace compact-master-detail ${detailOpen ? 'detail-open' : ''}`}>
          <aside className="managed-list" aria-label="组织管理条目列表">
            <div className="managed-list-header"><strong>全部条目</strong><span className="section-count">{items.length}</span></div>
            <div className="managed-list-body">
              {items.map(item => <ManagedListItem key={`${item.asset_kind}:${item.asset_key}`} item={item} local={localFor(item)} policyReady={policyReady} selected={`${item.asset_kind}:${item.asset_key}` === selectedKey} onSelect={() => { setSelectedKey(`${item.asset_kind}:${item.asset_key}`); setDetailOpen(true); }} />)}
            </div>
          </aside>
          <main className="managed-detail">
            <button className="workspace-back" onClick={() => setDetailOpen(false)}><ArrowLeft size={15} />返回全部条目</button>
            {selected ? <ManagedDetail item={selected} local={localFor(selected)} policyReady={policyReady} onRepair={onRepairPlugin} /> : null}
          </main>
        </section>
      )}
    </div>
  );
}

/// 三个类型共用一份清单时，行上必须标明类型，否则"文档解析"和"短视频创作"分不清是插件还是技能。
function asExtensionKind(value: string): ExtensionKind | null {
  return value === 'plugin' || value === 'skill' || value === 'workflow' ? value : null;
}

function assetKindLabel(item: ExtensionDesiredItem) {
  const kind = asExtensionKind(item.asset_kind);
  return kind ? extensionKindLabels[kind] : item.asset_kind;
}

function ManagedListItem({ item, local, policyReady, selected, onSelect }: { item: ExtensionDesiredItem; local: LocalInfo; policyReady: boolean; selected: boolean; onSelect: () => void }) {
  const compliant = policyReady && isCompliant(item, local);
  const label = policyReady ? stateLabel(item, local) : local.installed ? '本机已安装' : '本机未安装';
  const rail = policyReady
    ? compliant ? 'success' : item.desired_state === 'absent' ? 'danger' : 'warn'
    : local.needsRepair ? 'warn' : '';
  const pill = policyReady
    ? compliant ? 'success' : item.desired_state === 'absent' ? 'danger' : 'warn'
    : local.needsRepair ? 'warn' : 'neutral';
  const kind = asExtensionKind(item.asset_kind);
  return <button type="button" className={`managed-list-item ${selected ? 'selected' : ''}`} onClick={onSelect}>
    <span className={`managed-item-rail ${rail}`} />
    {/* 这一页一次混着三类能力，所以类型图标和左边的状态色条各管一件事。 */}
    {/* 认不出类型时留一个中性灰位，免得这一列忽有忽无、左右行对不齐。 */}
    {kind ? <ExtensionKindMark kind={kind} size={15} label={extensionKindLabels[kind]} /> : <span className="extension-kind-mark" aria-hidden="true" />}
    <span className="managed-item-copy"><strong>{item.name || item.asset_key}</strong><small>{assetKindLabel(item)} · {policyLabel(item)}</small><small>{localVersionLabel(local)}</small></span>
    <Pill kind={pill}>{label}</Pill>
  </button>;
}

function ManagedDetail({ item, local, policyReady, onRepair }: { item: ExtensionDesiredItem; local: LocalInfo; policyReady: boolean; onRepair?: (pluginId: string) => void }) {
  const compliant = policyReady && isCompliant(item, local);
  const state = policyReady ? stateLabel(item, local) : local.installed ? '本机已安装' : '本机未安装';
  /// 策略读不到时，「期望版本 / 可停用 / 可卸载」都只是本机推断，宁可留白也不冒充策略。
  const pending = '待核对';
  return <>
    <div className="managed-detail-header">
      <div className="managed-detail-title"><div className={`managed-detail-mark ${compliant ? 'success' : policyReady ? 'warn' : 'neutral'}`}>{compliant ? <CircleCheck size={19} /> : policyReady ? <CircleAlert size={19} /> : <ShieldCheck size={19} />}</div><div><div className="managed-title-line"><h3>{item.name || item.asset_key}</h3><Pill kind={policyReady ? compliant ? 'success' : 'warn' : 'neutral'}>{state}</Pill></div><code>{item.asset_key}</code></div></div>
      {local.needsRepair && onRepair ? <button className="btn btn-primary" type="button" title="清除失败记录并立即重新尝试调用" onClick={() => onRepair(item.asset_key)}><RefreshCw size={15} />修复并重试</button> : null}
    </div>
    <p className="managed-detail-description">{item.reason || '该能力由系统或组织策略统一管理。'}</p>
    {local.needsRepair ? <div className="plugin-local-error"><span>该插件最近调用失败，依赖它的技能会降级或不可用。</span>{local.failure ? <small>{local.failure}</small> : null}</div> : null}
    <div className="managed-detail-meta">
      <div><span>策略</span><strong>{policyLabel(item)}</strong></div>
      <div><span>来源</span><strong>{sourceLabel(item.source)}</strong></div>
      <div><span>期望版本</span><strong>{policyReady ? item.desired_version || '跟随策略' : pending}</strong></div>
      <div><span>本机版本</span><strong>{local.version || '未安装'}</strong></div>
    </div>
    <section className="managed-detail-section"><div className="skill-section-title"><div><ShieldCheck size={15} /><strong>治理边界</strong></div></div><div className="managed-policy-grid"><div><span>可停用</span><strong>{policyReady ? item.allow_disable === false ? '否' : '是' : pending}</strong></div><div><span>可卸载</span><strong>{policyReady ? item.allow_uninstall === false ? '否' : '是' : pending}</strong></div><div><span>安装方式</span><strong>{policyReady ? item.install_mode === 'silent' ? '自动安装' : '按需安装' : pending}</strong></div><div><span>组织说明</span><strong>{item.reason || '未提供'}</strong></div></div></section>
  </>;
}

function isManagedPolicy(item: ExtensionDesiredItem) {
  return item.management !== 'user_managed' || item.intent === 'required' || item.desired_state === 'absent';
}

/// 「我的能力 → 组织管理」页签的计数要与面板里看到的条目一致，所以这份清单对外导出。
export function managedItems(desired: ExtensionDesiredState | null, registry: PluginRegistry | null, skillStatus: CodexSkillStatusResponse | null) {
  const items = (desired?.items || []).filter(isManagedPolicy);
  const keys = new Set(items.map(item => `${item.asset_kind}:${item.asset_key}`));
  for (const plugin of registry?.items || []) {
    const key = `plugin:${plugin.id}`;
    if (!['required', 'managed', 'blocked'].includes(plugin.governance || '') || keys.has(key)) continue;
    const organizationManaged = plugin.governance === 'managed';
    const blocked = plugin.governance === 'blocked';
    items.push({
      product_id: plugin.id,
      asset_key: plugin.id,
      asset_kind: 'plugin',
      name: plugin.name || plugin.id,
      desired_state: blocked ? 'absent' : 'present',
      desired_version: plugin.version || '',
      desired_enabled: true,
      intent: 'required',
      management: organizationManaged || blocked ? 'organization_managed' : 'builtin',
      install_mode: blocked ? 'prompt' : 'silent',
      source: organizationManaged || blocked ? 'organization' : 'system',
      reason: blocked ? '该插件已被组织禁止' : organizationManaged ? '该插件由组织统一管理' : 'HiMind Agent 系统内置能力',
      allow_disable: false,
      allow_uninstall: blocked,
    });
    keys.add(key);
  }
  for (const skill of skillStatus?.items || []) {
    const manifest = skill.record.manifest;
    const key = `skill:${manifest.id}`;
    if (manifest.scope !== 'builtin' || keys.has(key)) continue;
    items.push({
      product_id: manifest.id,
      asset_key: manifest.id,
      asset_kind: 'skill',
      name: manifest.name,
      desired_state: 'present',
      desired_version: manifest.version,
      desired_enabled: true,
      intent: 'required',
      management: 'builtin',
      install_mode: 'silent',
      source: 'system',
      reason: 'HiMind Agent 系统内置技能',
      allow_disable: false,
      allow_uninstall: false,
    });
    keys.add(key);
  }
  return items;
}

function getLocalInfo(item: ExtensionDesiredItem, registry: PluginRegistry | null, skillStatus: CodexSkillStatusResponse | null, workflows: WorkflowCenterItem[]): LocalInfo {
  if (item.asset_kind === 'plugin') {
    const plugin = registry?.items?.find(candidate => candidate.id === item.asset_key);
    return { installed: Boolean(plugin?.version), enabled: plugin?.enabled !== false, version: plugin?.version || '', state: plugin?.status || (plugin?.version ? 'installed' : 'not_installed'), needsRepair: plugin ? pluginNeedsRepair(plugin) : false, failure: plugin?.error || '' };
  }
  if (item.asset_kind === 'workflow') {
    const workflow = workflows.find(candidate => candidate.package.id === item.asset_key);
    const installed = Boolean(workflow?.package.version);
    return { installed, enabled: workflow?.enabled !== false, version: workflow?.package.version || '', state: installed ? (workflow?.enabled ? 'installed' : 'disabled') : 'not_installed' };
  }
  const skill = skillStatus?.items?.find(candidate => candidate.record.manifest.id === item.asset_key);
  const installed = Boolean(skill && skill.client_state !== 'not_installed');
  return { installed, enabled: installed, version: skill?.installed_version || (installed && skill ? skill.record.manifest.version : ''), state: skill?.client_state || 'not_installed' };
}

function isCompliant(item: ExtensionDesiredItem, local: LocalInfo) {
  if (item.desired_state === 'absent') return !local.installed;
  if (item.desired_state === 'optional') return !local.needsRepair;
  // 插件运行异常也算「需处理」：策略上装好了、但依赖它的技能此刻是降级状态。
  return !local.needsRepair && local.installed && (!item.desired_version || item.desired_version === local.version) && (item.desired_enabled !== true || local.enabled);
}

function stateLabel(item: ExtensionDesiredItem, local: LocalInfo) {
  if (item.desired_state === 'absent') return local.installed ? '应移除' : '已阻止';
  if (item.desired_state === 'optional' && !local.installed) return '按需安装';
  if (!local.installed) return '待安装';
  if (local.needsRepair) return '运行异常';
  if (item.desired_version && item.desired_version !== local.version) return '版本不符';
  if (item.desired_enabled !== false && !local.enabled) return '已停用';
  return '已符合策略';
}

function localVersionLabel(local: LocalInfo) {
  if (!local.installed) return '本机未安装';
  return `本机 v${local.version || '--'} · ${local.state === 'modified' ? '有本地修改' : local.enabled ? '已启用' : '已停用'}`;
}

function policyLabel(item: ExtensionDesiredItem) {
  if (item.desired_state === 'absent') return '组织禁止';
  if (item.management === 'builtin') return '系统内置';
  if (item.intent === 'required') return '组织必装';
  if (item.management === 'organization_managed') return '组织管理';
  return '可选能力';
}

function sourceLabel(source?: string) {
  if (source === 'system') return '系统';
  if (source === 'organization') return '组织';
  if (source === 'marketplace') return '市场';
  return source || '未标注';
}
