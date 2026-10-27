import type { PluginQuickAccessView } from '../services/agentApi';

/** Stable destination identity for favorites and usage counters. */
export function quickAccessKey(view: PluginQuickAccessView) {
  return `${view.plugin_id}:${view.view_id}`;
}

/** Compact label shown in the toolbar and the all-tools list. */
export function quickAccessLabel(view: PluginQuickAccessView) {
  const label = view.short_title?.trim() || view.plugin_name?.trim() || view.title.trim();
  return label || '工具';
}

export function quickAccessLabelKey(view: PluginQuickAccessView) {
  return quickAccessLabel(view).replace(/\s+/g, ' ').toLocaleLowerCase('zh-CN');
}

export function compareQuickViews(left: PluginQuickAccessView, right: PluginQuickAccessView) {
  return left.order - right.order
    || left.short_title.localeCompare(right.short_title, 'zh-CN')
    || left.plugin_name.localeCompare(right.plugin_name, 'zh-CN')
    || left.view_id.localeCompare(right.view_id);
}

/**
 * A plugin can expose several navigation views with the same compact label. They
 * are distinct destinations technically, but indistinguishable in the toolbar
 * and all-tools list. Keep the first deterministic entry per plugin + label.
 * Different plugins may still use the same label; their plugin name remains the
 * secondary line and the toolbar applies one extra label-level pass below.
 */
export function dedupeQuickAccessViews(views: PluginQuickAccessView[]) {
  const seenDestinations = new Set<string>();
  const seenPluginLabels = new Set<string>();
  const result: PluginQuickAccessView[] = [];
  for (const view of [...views].sort(compareQuickViews)) {
    const destination = quickAccessKey(view);
    if (seenDestinations.has(destination)) continue;
    seenDestinations.add(destination);
    const pluginLabel = `${view.plugin_id}\u0000${quickAccessLabelKey(view)}`;
    if (seenPluginLabels.has(pluginLabel)) continue;
    seenPluginLabels.add(pluginLabel);
    result.push(view);
  }
  return result;
}

/** Toolbar uniqueness: preserve priority order, but never show the same label twice. */
export function pickUniqueQuickAccessLabels(views: PluginQuickAccessView[], limit: number) {
  const seenDestinations = new Set<string>();
  const seenLabels = new Set<string>();
  const result: PluginQuickAccessView[] = [];
  for (const view of views) {
    if (result.length >= limit) break;
    const destination = quickAccessKey(view);
    const label = quickAccessLabelKey(view);
    if (seenDestinations.has(destination) || seenLabels.has(label)) continue;
    seenDestinations.add(destination);
    seenLabels.add(label);
    result.push(view);
  }
  return result;
}
