import type { ReactNode } from 'react';
import { PageHeader } from '../components/Common';
import { capabilityKindIcons } from '../components/ExtensionKindMark';
import { capabilityKindLabels, capabilityKindOrder, type CapabilityKind } from '../data/extensionKinds';
import type { InstalledKind } from '../types';

/**
 * 「我的能力」= 已拥有的能力（自己装的 + 组织配发的）。
 *
 * 数字分身的心智里，"市场"回答"我还能获得什么"，这里回答"我现在会做什么"。
 * 插件 / 技能 / 工作流 / MCP 工具（+ 连接工作台时的组织管理）是同一个问题的几种口径，所以做成页内页签，
 * 每个类型仍然复用各自的管理界面（列表 + 详情 + 该类型自己的动作区）。
 */
export function InstalledPage({ kind, counts, dashboardEnabled, onSelectKind, children }: {
  kind: InstalledKind;
  counts: Record<InstalledKind, number>;
  dashboardEnabled: boolean;
  onSelectKind: (kind: InstalledKind) => void;
  children: ReactNode;
}) {
  // 页签是同一份类型口径的另一处落点：图标跟着类型走，四类各一个。
  const kinds: CapabilityKind[] = capabilityKindOrder.filter(item => item !== 'policy' || dashboardEnabled);
  return (
    <div className="installed-page">
      <PageHeader title="我的能力" description="自己安装的，也包括组织配发的。" />
      <div className="plugin-tabs installed-tabs" role="tablist" aria-label="我的能力类型">
        {kinds.map(item => {
          const KindIcon = capabilityKindIcons[item];
          return (
            <button
              key={item}
              type="button"
              role="tab"
              aria-selected={kind === item}
              className={kind === item ? 'active' : ''}
              onClick={() => onSelectKind(item)}
            >
              <KindIcon size={14} />{capabilityKindLabels[item]} <span>{counts[item] || 0}</span>
            </button>
          );
        })}
      </div>
      <div className="installed-body">{children}</div>
    </div>
  );
}
