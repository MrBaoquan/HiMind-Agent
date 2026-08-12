import { useEffect, useMemo, useState } from 'react';
import { CircleAlert, CircleCheck, ShieldCheck, Unplug } from 'lucide-react';
import { EmptyState, Pill } from '../components/Common';
import type { CodexSkillStatusResponse, ExtensionDesiredItem, ExtensionDesiredState, PluginRegistry } from '../services/agentApi';

export type ManagedCapabilityKind = 'plugin' | 'skill';

type ManagedCapabilitiesPageProps = {
  assetKind: ManagedCapabilityKind;
  desired: ExtensionDesiredState | null;
  loading: boolean;
  error: string | null;
  registry: PluginRegistry | null;
  skillStatus: CodexSkillStatusResponse | null;
};

type LocalInfo = {
  installed: boolean;
  enabled: boolean;
  version: string;
  state: string;
};

export function ManagedCapabilitiesPanel({ assetKind, desired, loading, error, registry, skillStatus }: ManagedCapabilitiesPageProps) {
  const items = useMemo(() => managedItems(desired, registry, skillStatus).filter(item => item.asset_kind === assetKind), [assetKind, desired, registry, skillStatus]);
  const [selectedKey, setSelectedKey] = useState('');
  useEffect(() => {
    if (!items.some(item => `${item.asset_kind}:${item.asset_key}` === selectedKey)) {
      setSelectedKey(items[0] ? `${items[0].asset_kind}:${items[0].asset_key}` : '');
    }
  }, [items, selectedKey]);
  const selected = items.find(item => `${item.asset_kind}:${item.asset_key}` === selectedKey) || items[0];
  const localFor = (item: ExtensionDesiredItem) => getLocalInfo(item, registry, skillStatus);
  const attentionCount = items.filter(item => !isCompliant(item, localFor(item))).length;
  const builtinCount = items.filter(item => policyLabel(item) === '系统内置').length;
  const requiredCount = items.filter(item => policyLabel(item) === '组织必装').length;
  const managedCount = items.filter(item => policyLabel(item) === '组织管理').length;

  if (loading && !desired) return <div className="page-loading"><span className="spinner" />正在读取系统内置{assetKind === 'plugin' ? '插件' : '技能'}</div>;

  return (
    <div className="managed-page managed-panel">
      {error ? <div className="blocker"><CircleAlert size={18} /><div><strong>系统内置数据暂时不可用</strong><span>{error}</span></div></div> : null}
      <section className="managed-summary" aria-label="系统内置摘要">
        <div><span>系统内置</span><strong>{builtinCount}</strong></div>
        <div><span>组织必装</span><strong>{requiredCount}</strong></div>
        <div><span>组织管理</span><strong>{managedCount}</strong></div>
        <div><span>需处理</span><strong className={attentionCount ? 'warning-text' : ''}>{attentionCount}</strong></div>
        <div className="managed-summary-note"><ShieldCheck size={16} /><span>{desired?.generation ? `策略版本 ${desired.generation}` : '尚未获取策略版本'}</span></div>
      </section>

      {!items.length ? (
        <div className="managed-empty"><EmptyState icon={Unplug} title={`当前没有系统内置${assetKind === 'plugin' ? '插件' : '技能'}`} text={error ? '请完成 Agent 配对后重新刷新。' : '系统内置和组织策略条目会显示在这里。'} /></div>
      ) : (
        <section className="managed-workspace">
          <aside className="managed-list" aria-label="系统内置条目列表">
            <div className="managed-list-header"><strong>全部条目</strong><span className="section-count">{items.length}</span></div>
            <div className="managed-list-body">
              {items.map(item => <ManagedListItem key={`${item.asset_kind}:${item.asset_key}`} item={item} local={localFor(item)} selected={`${item.asset_kind}:${item.asset_key}` === selectedKey} onSelect={() => setSelectedKey(`${item.asset_kind}:${item.asset_key}`)} />)}
            </div>
          </aside>
          <main className="managed-detail">
            {selected ? <ManagedDetail item={selected} local={localFor(selected)} /> : null}
          </main>
        </section>
      )}
    </div>
  );
}

function ManagedListItem({ item, local, selected, onSelect }: { item: ExtensionDesiredItem; local: LocalInfo; selected: boolean; onSelect: () => void }) {
  const compliant = isCompliant(item, local);
  const label = stateLabel(item, local);
  return <button type="button" className={`managed-list-item ${selected ? 'selected' : ''}`} onClick={onSelect}>
    <span className={`managed-item-rail ${compliant ? 'success' : item.desired_state === 'absent' ? 'danger' : 'warn'}`} />
    <span className="managed-item-copy"><strong>{item.name || item.asset_key}</strong><small>{item.asset_kind === 'plugin' ? '插件' : '技能'} · {policyLabel(item)}</small><small>{localVersionLabel(local)}</small></span>
    <Pill kind={compliant ? 'success' : item.desired_state === 'absent' ? 'danger' : 'warn'}>{label}</Pill>
  </button>;
}

function ManagedDetail({ item, local }: { item: ExtensionDesiredItem; local: LocalInfo }) {
  const compliant = isCompliant(item, local);
  return <>
    <div className="managed-detail-header">
      <div className="managed-detail-title"><div className={`managed-detail-mark ${compliant ? 'success' : 'warn'}`}>{compliant ? <CircleCheck size={19} /> : <CircleAlert size={19} />}</div><div><div className="managed-title-line"><h3>{item.name || item.asset_key}</h3><Pill kind={compliant ? 'success' : 'warn'}>{stateLabel(item, local)}</Pill></div><code>{item.asset_key}</code></div></div>
    </div>
    <p className="managed-detail-description">{item.reason || '该能力由系统或组织策略统一管理。'}</p>
    <div className="managed-detail-meta">
      <div><span>策略</span><strong>{policyLabel(item)}</strong></div>
      <div><span>来源</span><strong>{sourceLabel(item.source)}</strong></div>
      <div><span>期望版本</span><strong>{item.desired_version || '跟随策略'}</strong></div>
      <div><span>本机版本</span><strong>{local.version || '未安装'}</strong></div>
    </div>
    <section className="managed-detail-section"><div className="skill-section-title"><div><ShieldCheck size={15} /><strong>治理边界</strong></div></div><div className="managed-policy-grid"><div><span>可停用</span><strong>{item.allow_disable === false ? '否' : '是'}</strong></div><div><span>可卸载</span><strong>{item.allow_uninstall === false ? '否' : '是'}</strong></div><div><span>安装方式</span><strong>{item.install_mode === 'silent' ? '自动安装' : '按需安装'}</strong></div><div><span>组织说明</span><strong>{item.reason || '未提供'}</strong></div></div></section>
  </>;
}

function isManagedPolicy(item: ExtensionDesiredItem) {
  return item.management !== 'user_managed' || item.intent === 'required' || item.desired_state === 'absent';
}

function managedItems(desired: ExtensionDesiredState | null, registry: PluginRegistry | null, skillStatus: CodexSkillStatusResponse | null) {
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

function getLocalInfo(item: ExtensionDesiredItem, registry: PluginRegistry | null, skillStatus: CodexSkillStatusResponse | null): LocalInfo {
  if (item.asset_kind === 'plugin') {
    const plugin = registry?.items?.find(candidate => candidate.id === item.asset_key);
    return { installed: Boolean(plugin?.version), enabled: plugin?.enabled !== false, version: plugin?.version || '', state: plugin?.status || (plugin?.version ? 'installed' : 'not_installed') };
  }
  const skill = skillStatus?.items?.find(candidate => candidate.record.manifest.id === item.asset_key);
  const installed = Boolean(skill && skill.client_state !== 'not_installed');
  return { installed, enabled: installed, version: skill?.installed_version || (installed && skill ? skill.record.manifest.version : ''), state: skill?.client_state || 'not_installed' };
}

function isCompliant(item: ExtensionDesiredItem, local: LocalInfo) {
  if (item.desired_state === 'absent') return !local.installed;
  if (item.desired_state === 'optional') return true;
  return local.installed && (!item.desired_version || item.desired_version === local.version) && (item.desired_enabled !== true || local.enabled);
}

function stateLabel(item: ExtensionDesiredItem, local: LocalInfo) {
  if (item.desired_state === 'absent') return local.installed ? '应移除' : '已阻止';
  if (item.desired_state === 'optional' && !local.installed) return '按需安装';
  if (!local.installed) return '待安装';
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
