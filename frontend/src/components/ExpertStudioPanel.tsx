import { useEffect, useRef, useState } from 'react';
import { Code2, Download, FileUp, FolderPlus, Github, GraduationCap, MoreHorizontal, Plus, Send, X } from 'lucide-react';
import { EmptyState, Pill } from './Common';
import { ActionMenu, ActionMenuItem } from './ActionMenu';
import { agentApi, type ExpertCatalogItem, type ExpertSummary } from '../services/agentApi';

type ExpertStudioPanelProps = {
  experts: ExpertSummary[];
  expertCatalog?: ExpertCatalogItem[];
  activeExpert: string;
  compact?: boolean;
  onRefresh: () => Promise<void> | void;
  onActivate: (id: string, version: string) => Promise<void>;
  onNotify: (message: string, tone?: 'success' | 'error') => void;
  onInstallMarketExpert?: (item: ExpertCatalogItem) => Promise<void>;
  /** 把专家复制成工作区里可编辑的项目（`experts/<slug>/`）。 */
  onMaterializeToWorkspace?: (expert: ExpertSummary) => Promise<void>;
  showAuthoring?: boolean;
  showMarket?: boolean;
  workspaceRoot?: string;
  showProjection?: boolean;
};

function readableError(error: unknown) {
  if (error instanceof Error && error.message) return error.message;
  return typeof error === 'string' && error.trim() ? error : '操作未完成';
}

function clientLabel(client: string) {
  return ({
    'himind-dsh': 'HiMind',
    codex: 'Codex',
    'github-copilot': 'GitHub Copilot',
    'claude-code': 'Claude Code',
    cursor: 'Cursor',
    windsurf: 'Windsurf',
    cline: 'Cline',
    portable: '通用格式',
  } as Record<string, string>)[client] || client;
}

function expertMeta(expert: ExpertSummary) {
  const author = expert.author || '未注明作者';
  const targets = expert.supported_clients.map(clientLabel).filter(label => label !== author);
  return [author, targets.length ? targets.join('、') : null].filter(Boolean).join(' · ');
}

export function ExpertStudioPanel({ experts, expertCatalog = [], activeExpert, compact = false, onRefresh, onActivate, onNotify, onInstallMarketExpert, onMaterializeToWorkspace, showAuthoring = true, showMarket = true, workspaceRoot = '', showProjection = false }: ExpertStudioPanelProps) {
  const [authoring, setAuthoring] = useState(false);
  const [busy, setBusy] = useState('');
  const [draft, setDraft] = useState({ id: '', name: '', version: '1.0.0', description: '', instructions: '' });
  const createTriggerRef = useRef<HTMLButtonElement | null>(null);
  const modalRef = useRef<HTMLElement | null>(null);
  const wasAuthoring = useRef(false);

  useEffect(() => {
    if (!authoring) {
      if (wasAuthoring.current) createTriggerRef.current?.focus();
      wasAuthoring.current = false;
      return;
    }
    wasAuthoring.current = true;
    const dialog = modalRef.current;
    if (!dialog) return;
    const focusable = () => Array.from(dialog.querySelectorAll<HTMLElement>('button, input, textarea, select, [href], [tabindex]:not([tabindex="-1"])')).filter(item => !item.hasAttribute('disabled'));
    focusable()[0]?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') { event.preventDefault(); setAuthoring(false); return; }
      if (event.key !== 'Tab') return;
      const items = focusable();
      if (!items.length) return;
      const first = items[0];
      const last = items[items.length - 1];
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [authoring]);

  const importPackage = async () => {
    const path = await agentApi.pickExpertPackage();
    if (!path) return;
    setBusy('import');
    try {
      await agentApi.importExpertPackage(path);
      await onRefresh();
      onNotify('专家已导入', 'success');
    } catch (error) {
      onNotify(readableError(error), 'error');
    } finally { setBusy(''); }
  };

  const exportPackage = async (expert: ExpertSummary) => {
    setBusy(`export:${expert.id}`);
    try {
      await agentApi.exportExpertPackage(expert.id, expert.version);
      onNotify('专家包已导出', 'success');
    } catch (error) {
      if (!readableError(error).includes('取消')) onNotify(readableError(error), 'error');
    } finally { setBusy(''); }
  };

  const create = async () => {
    if (!draft.id.trim() || !draft.name.trim() || !draft.instructions.trim()) {
      onNotify('请填写专家名称和工作说明', 'error');
      return;
    }
    setBusy('create');
    try {
      await agentApi.saveExpert({
        id: draft.id.trim(), name: draft.name.trim(), version: draft.version.trim() || '1.0.0',
        description: draft.description.trim(), instructions: draft.instructions.trim(),
        supported_clients: ['himind-dsh', 'codex', 'github-copilot', 'claude-code', 'portable'],
      });
      await onRefresh();
      setAuthoring(false);
      setDraft({ id: '', name: '', version: '1.0.0', description: '', instructions: '' });
      onNotify('专家已创建', 'success');
    } catch (error) { onNotify(readableError(error), 'error'); }
    finally { setBusy(''); }
  };

  const materialize = async (expert: ExpertSummary) => {
    if (!onMaterializeToWorkspace) return;
    if (!workspaceRoot.trim()) {
      onNotify('请先选择扩展工作区', 'error');
      return;
    }
    setBusy(`materialize:${expert.id}`);
    try {
      await onMaterializeToWorkspace(expert);
      onNotify(`${expert.name} 已复制到工作区`, 'success');
    } catch (error) {
      onNotify(readableError(error), 'error');
    } finally { setBusy(''); }
  };

  const project = async (expert: ExpertSummary, clientId: string) => {
    if (!workspaceRoot.trim()) {
      onNotify('请先选择项目工作区', 'error');
      return;
    }
    setBusy(`project:${expert.id}:${clientId}`);
    try {
      const receipt = await agentApi.projectExpertToClient(expert.id, clientId, workspaceRoot, expert.version);
      onNotify(receipt.changed === false
        ? `${expert.name} 已是最新文件`
        : `${expert.name} 已同步到 ${clientLabel(clientId)}；客户端加载状态由客户端确认`, 'success');
    } catch (error) {
      onNotify(readableError(error), 'error');
    } finally { setBusy(''); }
  };

  return <section className={`expert-studio-panel${compact ? ' compact' : ''}`}>
    <div className="expert-studio-head">
      <div><h3><GraduationCap size={15} />专家</h3><span>供 AI 客户端选择的工作角色和工作标准。</span></div>
      <div className="expert-studio-actions">
        <button type="button" className="btn" disabled={busy === 'import'} onClick={() => void importPackage()}><FileUp size={14} />导入</button>
        {showAuthoring ? <button ref={createTriggerRef} type="button" className="btn btn-primary" onClick={() => setAuthoring(true)}><Plus size={14} />创建专家</button> : null}
      </div>
    </div>
    {experts.length ? <div className="expert-studio-list">{experts.map(expert => {
      const active = activeExpert === `${expert.id}@${expert.version}`;
      // 专家没有"启用/停用"这种持久状态：用不用由会话里的 AI 客户端按任务决定，
      // 所以卡片只描述这个专家是什么，以及能同步到哪些客户端。
      return <article className="expert-studio-item" key={`${expert.id}@${expert.version}`}>
        <div className="expert-studio-copy"><div><strong title={expert.name}>{expert.name}</strong><Pill kind="neutral">v{expert.version}</Pill></div><p title={expert.description || undefined}>{expert.description || '可复用的专家工作标准。'}</p><small>{expertMeta(expert)}</small></div>
        <div className="expert-studio-item-actions">
          {showProjection ? <ActionMenu label="同步到客户端" icon={<Send size={14} />} title={`同步${expert.name}到客户端`} disabled={Boolean(busy)}>
            {close => <>
              <ActionMenuItem icon={<Github size={14} />} label="GitHub Copilot" onClick={() => { close(); void project(expert, 'github-copilot'); }} />
              <ActionMenuItem icon={<Code2 size={14} />} label="Codex" onClick={() => { close(); void project(expert, 'codex'); }} />
              <ActionMenuItem icon={<Code2 size={14} />} label="Claude Code" onClick={() => { close(); void project(expert, 'claude-code'); }} />
              <ActionMenuItem icon={<Code2 size={14} />} label="Cursor" onClick={() => { close(); void project(expert, 'cursor'); }} />
              <ActionMenuItem icon={<Code2 size={14} />} label="Windsurf" onClick={() => { close(); void project(expert, 'windsurf'); }} />
              <ActionMenuItem icon={<Code2 size={14} />} label="Cline" onClick={() => { close(); void project(expert, 'cline'); }} />
            </>}
          </ActionMenu> : null}
          <ActionMenu icon={<MoreHorizontal size={14} />} variant="icon" title="更多操作" disabled={Boolean(busy)}>
            {close => <>
              {onMaterializeToWorkspace ? <ActionMenuItem icon={<FolderPlus size={14} />} label="复制到工作区" title="在工作区里生成一份可编辑的专家项目" onClick={() => { close(); void materialize(expert); }} /> : null}
              <ActionMenuItem icon={<Download size={14} />} label="导出专家包" onClick={() => { close(); void exportPackage(expert); }} />
            </>}
          </ActionMenu>
        </div>
      </article>;
    })}</div> : <EmptyState icon={GraduationCap} title="还没有专家" text="创建或导入一个专家后，就能在 AI 对话中使用。" />}
    {showMarket && expertCatalog.length && onInstallMarketExpert ? <div className="expert-market-list"><div className="expert-market-title">市场专家</div>{expertCatalog.map(item => <article className="expert-studio-item" key={`${item.expert_id}@${item.version}`}><div className="expert-studio-copy"><div><strong title={item.name}>{item.name}</strong><Pill kind="neutral">v{item.version}</Pill></div><p title={item.description || undefined}>{item.description}</p><small>{item.author_name || '发布者未注明'}</small></div><div className="expert-studio-item-actions"><button type="button" className="btn btn-primary" disabled={Boolean(busy)} onClick={() => { setBusy(`market:${item.expert_id}`); void onInstallMarketExpert(item).finally(() => setBusy('')); }}>安装</button></div></article>)}</div> : null}
    {authoring ? <div className="expert-studio-modal-backdrop" role="presentation"><section ref={modalRef} tabIndex={-1} className="expert-studio-modal" role="dialog" aria-modal="true" aria-labelledby="expert-studio-title">
      <header><strong id="expert-studio-title">创建专家</strong><button type="button" className="btn btn-icon" aria-label="关闭" onClick={() => setAuthoring(false)}><X size={15} /></button></header>
      <label>名称<input value={draft.name} onChange={event => setDraft({ ...draft, name: event.target.value })} placeholder="例如：产品经理" /></label>
      <label>ID<input value={draft.id} onChange={event => setDraft({ ...draft, id: event.target.value })} placeholder="com.example.expert.product-manager" /></label>
      <label>版本<input value={draft.version} onChange={event => setDraft({ ...draft, version: event.target.value })} /></label>
      <label>简介<input value={draft.description} onChange={event => setDraft({ ...draft, description: event.target.value })} placeholder="这个专家负责什么工作" /></label>
      <label>工作说明<textarea rows={6} value={draft.instructions} onChange={event => setDraft({ ...draft, instructions: event.target.value })} placeholder="说明它如何思考、执行和交付。" /></label>
      <footer><button type="button" className="btn" onClick={() => setAuthoring(false)}>取消</button><button type="button" className="btn btn-primary" disabled={busy === 'create'} onClick={() => void create()}>保存专家</button></footer>
    </section></div> : null}
  </section>;
}
