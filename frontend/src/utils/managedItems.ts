import type { CodexSkillStatusResponse, ExtensionDesiredItem, ExtensionDesiredState, PluginRegistry } from '../services/agentApi';

function isManagedPolicy(item: ExtensionDesiredItem) {
  return item.management !== 'user_managed' || item.intent === 'required' || item.desired_state === 'absent';
}

/** Counts the same organization-managed inventory shown by the lazy panel. */
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
