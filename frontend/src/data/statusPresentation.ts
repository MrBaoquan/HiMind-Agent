/**
 * User-facing status vocabulary for extension surfaces.
 *
 * Status is not a free-form sentence: a page may need to know which state wins
 * when ownership, update availability and runtime health disagree. Keeping the
 * labels and priority here prevents "可更新/有更新/待更新" and
 * "已安装/本机已安装/当前" from drifting across pages.
 */
export type ExtensionStatusTone = 'success' | 'warn' | 'danger' | 'neutral';
export type ExtensionStatusKey =
  | 'blocked'
  | 'error'
  | 'disabled'
  | 'update'
  | 'modified'
  | 'sync-required'
  | 'installed'
  | 'not-installed'
  | 'built'
  | 'unbuilt';

export type ExtensionStatusPresentation = {
  key: ExtensionStatusKey;
  label: string;
  tone: ExtensionStatusTone;
  priority: number;
};

const definitions: Record<ExtensionStatusKey, Omit<ExtensionStatusPresentation, 'key'>> = {
  blocked: { label: '不可用', tone: 'danger', priority: 100 },
  error: { label: '运行异常', tone: 'danger', priority: 90 },
  disabled: { label: '已停用', tone: 'warn', priority: 80 },
  update: { label: '可更新', tone: 'warn', priority: 70 },
  modified: { label: '已修改', tone: 'warn', priority: 60 },
  'sync-required': { label: '需重新同步', tone: 'warn', priority: 60 },
  built: { label: '已构建', tone: 'success', priority: 30 },
  installed: { label: '已安装', tone: 'success', priority: 20 },
  unbuilt: { label: '未构建', tone: 'neutral', priority: 10 },
  'not-installed': { label: '未安装', tone: 'neutral', priority: 0 },
};

/** A version row is already scoped to one extension; "当前" is shorter and
 * unambiguous than repeating "本机已安装" under every installed version. */
export const VERSION_CURRENT_LABEL = '当前';

export function extensionStatus(key: ExtensionStatusKey): ExtensionStatusPresentation {
  return { key, ...definitions[key] };
}

/** Return the one status that should own a row when several facts coexist. */
export function primaryExtensionStatus(keys: ExtensionStatusKey[]): ExtensionStatusPresentation | null {
  if (!keys.length) return null;
  return keys
    .map(extensionStatus)
    .sort((left, right) => right.priority - left.priority)[0] ?? null;
}