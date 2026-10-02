import { useCallback } from 'react';
import { ArrowUpRight, Blocks, X } from 'lucide-react';
import { McpConnectionsPanel } from './McpConnectionsPanel';
import { useMcpManager } from './useMcpManager';

type Props = {
  open: boolean;
  /** HiMind AI 实际能加载的技能数；未读到时为 null。 */
  skillCount: number | null;
  onClose: () => void;
  /** 连接被增删改之后要让会话重连：运行时和工具上下文都要重新取一遍。 */
  onRuntimeChanged: () => void;
  onToolContextChanged: () => void;
  onOpenCapabilities: () => void;
  /** 挑工具的地方挪到了市场（「MCP 工具」页签），这里只留一个入口。 */
  onOpenMcpTools: () => void;
};

/**
 * HiMind AI 会话旁的快捷管理：只处理「对话里现在能用哪些工具」。
 *
 * 挑选和安装工具是低频的浏览动作，归到市场的「MCP 工具」页签；
 * 这个对话框回答的是会话现场的问题——挂上了什么、能不能连上、要不要停用，
 * 所以只铺已添加连接，目录用一句入口带过去。
 */
export function BuiltinAiExtensionsDialog({
  open,
  skillCount,
  onClose,
  onRuntimeChanged,
  onToolContextChanged,
  onOpenCapabilities,
  onOpenMcpTools,
}: Props) {
  const changed = useCallback(() => {
    onToolContextChanged();
    onRuntimeChanged();
  }, [onRuntimeChanged, onToolContextChanged]);
  // 会话旁不需要工具目录，读它是白花一次请求；`active` 保证对话框关着时不发请求。
  const mcp = useMcpManager({ active: open, withCatalog: false, onChanged: changed });

  if (!open) return null;

  return (
    <div className="modal-backdrop builtin-ai-extension-backdrop" role="presentation" onMouseDown={event => { if (event.currentTarget === event.target) onClose(); }}>
      <section className="builtin-ai-extension-dialog" role="dialog" aria-modal="true" aria-labelledby="builtin-ai-extension-title">
        <header className="builtin-ai-extension-header">
          <div><span className="builtin-ai-extension-mark"><Blocks size={18} /></span><div><h3 id="builtin-ai-extension-title">HiMind AI 工具</h3><p>给 HiMind AI 挂上对话里可以调用的本机工具</p></div></div>
          <button type="button" className="btn btn-icon" title="关闭" aria-label="关闭" onClick={onClose}><X size={16} /></button>
        </header>
        <div className="builtin-ai-mcp-panel" ref={mcp.panelRef}>
          <McpConnectionsPanel
            mcp={mcp}
            onBrowseCatalog={() => { onClose(); onOpenMcpTools(); }}
            note="这些工具会在对话里提供给 HiMind AI 调用，停用后不再加载。"
          />
        </div>
        <footer className="builtin-ai-extension-footer">
          <span className="builtin-ai-extension-footer-note">{skillCount && skillCount > 0 ? `HiMind AI 会直接调用已安装的 ${skillCount} 项技能和插件` : 'HiMind AI 会直接调用已安装的技能和插件'}</span>
          <div className="actions-row">
            {/* 空态里已经给了「浏览 MCP 工具」，页脚再放一次就是同一个动作说两遍。 */}
            <button type="button" className="btn" onClick={onOpenCapabilities}><ArrowUpRight size={14} />打开我的能力</button>
          </div>
        </footer>
      </section>
    </div>
  );
}
