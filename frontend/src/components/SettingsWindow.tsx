import type { ReactNode } from 'react';
import { useEffect, useMemo, useState } from 'react';
import { Search, X } from 'lucide-react';
import { SETTINGS_RAIL_GROUPS, settingsRailItemMatch, settingsRailItemMatches, splitSettingsRailLabel, type SettingsRailKey } from '../settingsModel';

type SettingsWindowProps = {
  activeKey: SettingsRailKey;
  onSelect: (key: SettingsRailKey) => void;
  children: ReactNode;
};

/**
 * Low-frequency management surfaces live in a dedicated desktop window so the
 * main work navigation can stay focused on AI, work, automation and extensions.
 * One vertical rail owns every surface and the right pane renders exactly one
 * of them, matching how ChatGPT and other desktop tools structure settings.
 */
export function SettingsWindow({ activeKey, onSelect, children }: SettingsWindowProps) {
  return (
    <div className="settings-window-root">
      <SettingsRail activeKey={activeKey} onSelect={onSelect} />
      {/* 容器查询要量的是「整列可用宽度」，和主窗口 .main 一样把内边距算进去，
          否则阈值会比主窗口提前约 56px 命中，通用/审批这些页在默认尺寸下就被
          压成竖排。所以内边距挪到里层，外层只负责滚动与容器身份。 */}
      <main className="settings-window-content" aria-label="设置内容">
        <div className="settings-window-pane">{children}</div>
      </main>
    </div>
  );
}

/**
 * 条目收敛之后左栏只有 8 条，但用户脑子里的词还是旧的（SVN、Unity、日志）。
 * 检索走 keywords 别名，不靠把菜单拆回去找回发现性。
 */
function SettingsRail({ activeKey, onSelect }: { activeKey: SettingsRailKey; onSelect: (key: SettingsRailKey) => void }) {
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState('');
  // 输入即过滤会让左栏每敲一个字都重排一次；200ms 足够盖住连续输入。
  useEffect(() => {
    const timer = window.setTimeout(() => setFilter(query.trim()), 200);
    return () => window.clearTimeout(timer);
  }, [query]);

  const groups = useMemo(() => SETTINGS_RAIL_GROUPS
    .map(group => ({ label: group.label, items: group.items.filter(item => settingsRailItemMatches(item, filter)) }))
    .filter(group => group.items.length > 0), [filter]);
  const matchCount = groups.reduce((total, group) => total + group.items.length, 0);
  const firstMatch = groups[0]?.items[0]?.key;
  const searching = filter.length > 0;

  return (
    <aside className="settings-rail" aria-label="设置导航">
      <div className="settings-rail-heading">设置</div>
      <label className="settings-rail-search">
        <Search size={14} aria-hidden="true" />
        <span className="sr-only">搜索设置</span>
        <input
          type="search"
          value={query}
          placeholder="搜索设置"
          autoComplete="off"
          onChange={event => setQuery(event.target.value)}
          onKeyDown={event => {
            if (event.key === 'Escape') {
              event.preventDefault();
              setQuery('');
              return;
            }
            // 回车直接进第一条命中项，省掉「搜索 → 看清 → 再点」这一步。
            if (event.key === 'Enter' && firstMatch) {
              event.preventDefault();
              onSelect(firstMatch);
            }
          }}
        />
        {query ? <button type="button" className="settings-rail-search-clear" title="清除搜索" aria-label="清除搜索" onClick={() => setQuery('')}><X size={13} /></button> : null}
      </label>
      <nav className="settings-rail-nav" aria-label="设置分类">
        {groups.map(group => (
          <div className="settings-rail-group" key={group.label}>
            <span className="settings-rail-group-label">{group.label}</span>
            {group.items.map(item => {
              const Icon = item.icon;
              const active = activeKey === item.key;
              const alias = settingsRailItemMatch(item, filter).alias;
              return (
                <button
                  type="button"
                  key={item.key}
                  className={active ? 'active' : ''}
                  onClick={() => onSelect(item.key)}
                  aria-current={active ? 'page' : undefined}
                  title={item.label}
                >
                  <Icon size={16} strokeWidth={1.8} aria-hidden="true" />
                  <span className="settings-rail-item-text">
                    <span>
                      {splitSettingsRailLabel(item.label, filter).map((part, index) => (
                        part.hit ? <mark key={index}>{part.text}</mark> : <span key={index}>{part.text}</span>
                      ))}
                    </span>
                    {alias ? <small className="settings-rail-alias">匹配「{alias}」</small> : null}
                  </span>
                </button>
              );
            })}
          </div>
        ))}
      </nav>
      {searching && !matchCount ? <p className="settings-rail-empty">没有匹配的设置</p> : null}
      <span className="sr-only" role="status" aria-live="polite">{searching ? `${matchCount} 项设置` : ''}</span>
    </aside>
  );
}
