import { useEffect, useState, type FormEvent } from 'react';
import { CheckCircle2, CircleAlert, FileCode2, FolderPlus, PackagePlus, RefreshCw, RotateCcw, ShieldCheck, Upload } from 'lucide-react';
import { BusyIndicator } from './BusyIndicator';
import { Pill } from './Common';
import { useConfirm } from './ConfirmDialog';
import { agentApi, type InstructionPackDraft, type InstructionPackDraftInput, type InstructionPackTestResult, type InstructionProjectionPlan, type InstructionProjectionReceipt, type InstructionTargetDescriptor } from '../services/agentApi';

type InstructionProjectionPanelProps = {
  workspaceRoot: string;
  disabled?: boolean;
  /** 作为「项目规则」页签的内容区渲染：不出折叠头，直接展开。 */
  embedded?: boolean;
  /** 规则库增删或发布后回调，供外层刷新页签计数。 */
  onLibraryChanged?: () => void;
  /** 是否显示「同步到客户端」页签；「我的能力」里只管理规则库，不涉及某个工作区。 */
  clientsTab?: boolean;
  /** 把某个规则版本复制成工作区里可编辑的项目（`rules/<slug>/`）。 */
  onMaterializeToWorkspace?: (draft: InstructionPackDraft) => Promise<void>;
};

function statusLabel(status: string) {
  return ({
    native_loaded: '原生目标',
    projected_managed: '已同步',
    projected_degraded: '兼容同步',
    conflict: '存在冲突',
    blocked: '暂不可用',
  } as Record<string, string>)[status] || status;
}

function statusTone(status: string): 'success' | 'warn' | 'danger' | 'neutral' {
  if (status === 'native_loaded' || status === 'projected_managed') return 'success';
  if (status === 'conflict' || status === 'blocked') return 'danger';
  if (status === 'projected_degraded') return 'warn';
  return 'neutral';
}

function targetLabel(target: InstructionTargetDescriptor) {
  if (target.target.client_id === 'claude-code') return target.target.scope === 'global' ? 'Claude Code · 用户规则' : 'Claude Code · 项目规则';
  if (target.target.client_id === 'github-copilot') return target.degraded ? 'GitHub Copilot · 兼容同步' : 'GitHub Copilot · 仓库规则';
  return target.target.scope === 'global' ? 'Codex · 用户规则' : 'Codex · 项目规则';
}

function targetState(target: InstructionTargetDescriptor) {
  if (target.receipt) return '已有同步记录';
  if (!target.detected) return '未发现文件';
  if (target.degraded) return '兼容候选';
  return '已发现文件';
}

export function InstructionProjectionPanel({ workspaceRoot, disabled = false, embedded = false, onLibraryChanged, onMaterializeToWorkspace, clientsTab = true }: InstructionProjectionPanelProps) {
  const confirm = useConfirm();
  const [open, setOpen] = useState(false);
  const [activeView, setActiveView] = useState<'rules' | 'clients'>('rules');
  const [targets, setTargets] = useState<InstructionTargetDescriptor[]>([]);
  const [plans, setPlans] = useState<Record<string, InstructionProjectionPlan>>({});
  const [receipts, setReceipts] = useState<Record<string, InstructionProjectionReceipt>>({});
  const [packDrafts, setPackDrafts] = useState<InstructionPackDraft[]>([]);
  const [selectedPacks, setSelectedPacks] = useState<Record<string, string[]>>({});
  const [packBusy, setPackBusy] = useState(false);
  const [packMessage, setPackMessage] = useState('');
  const [authoring, setAuthoring] = useState(false);
  const [authoringInput, setAuthoringInput] = useState<InstructionPackDraftInput>({
    id: 'com.himind.instruction.new-pack',
    name: '',
    version: '1.0.0',
    description: '',
    release_notes: '首次发布',
    instructions: '# 工作规则\n\n',
    source: 'local_authoring',
  });
  const [loading, setLoading] = useState(false);
  const [busyKey, setBusyKey] = useState('');
  const [error, setError] = useState('');

  const loadTargets = async () => {
    setLoading(true);
    setError('');
    try {
      const discovered = clientsTab && workspaceRoot.trim() ? await agentApi.instructionTargets(workspaceRoot) : [];
      setTargets(discovered);
      setReceipts(discovered.reduce<Record<string, InstructionProjectionReceipt>>((current, item) => {
        if (item.receipt) current[item.target.adapter_id] = item.receipt;
        return current;
      }, {}));
      setPlans({});
      setPackDrafts(await agentApi.instructionPackDrafts());
      setSelectedPacks({});
      onLibraryChanged?.();
    } catch (reason) {
      setError(typeof reason === 'string' ? reason : '读取客户端规则目标失败');
    } finally {
      setLoading(false);
    }
  };

  const importInstructionPack = async () => {
    const path = await agentApi.pickInstructionFile();
    if (!path) return;
    setPackBusy(true);
    setPackMessage('');
    try {
      const draft = await agentApi.importInstructionFile(path);
      setPackDrafts(current => [draft, ...current.filter(item => `${item.manifest.id}:${item.manifest.version}` !== `${draft.manifest.id}:${draft.manifest.version}`)]);
      setPackMessage(`已创建项目规则草稿：${draft.manifest.name} v${draft.manifest.version}`);
    } catch (reason) {
      setError(typeof reason === 'string' ? reason : '导入规则文件失败');
    } finally {
      setPackBusy(false);
    }
  };

  const importInstructionPackage = async () => {
    const path = await agentApi.pickInstructionPackage();
    if (!path) return;
    setPackBusy(true);
    setPackMessage('');
    try {
      const draft = await agentApi.importInstructionPackage(path);
      setPackDrafts(current => [draft, ...current.filter(item => `${item.manifest.id}:${item.manifest.version}` !== `${draft.manifest.id}:${draft.manifest.version}`)]);
      setPackMessage(`已导入项目规则：${draft.manifest.name} v${draft.manifest.version}，请先检查`);
    } catch (reason) {
      setError(typeof reason === 'string' ? reason : '导入项目规则失败');
    } finally {
      setPackBusy(false);
    }
  };

  const saveInstructionPack = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setPackBusy(true);
    setPackMessage('');
    setError('');
    try {
      const draft = await agentApi.saveInstructionPackDraft(authoringInput);
      setPackDrafts(current => [draft, ...current.filter(item => `${item.manifest.id}:${item.manifest.version}` !== `${draft.manifest.id}:${draft.manifest.version}`)]);
      setAuthoring(false);
      setPackMessage(`已创建项目规则草稿：${draft.manifest.name} v${draft.manifest.version}`);
    } catch (reason) {
      setError(typeof reason === 'string' ? reason : '创建项目规则失败');
    } finally {
      setPackBusy(false);
    }
  };

  const testInstructionPack = async (draft: InstructionPackDraft) => {
    setPackBusy(true);
    setPackMessage('');
    try {
      const result: InstructionPackTestResult = await agentApi.testInstructionPackDraft(draft.manifest.id, draft.manifest.version);
      setPackDrafts(current => current.map(item => item.manifest.id === draft.manifest.id && item.manifest.version === draft.manifest.version ? result.draft : item));
      setPackMessage(result.readiness === 'ready' ? '项目规则检查通过' : `项目规则检查未通过：${result.issues.join('；')}`);
    } catch (reason) {
      setError(typeof reason === 'string' ? reason : '项目规则检查失败');
    } finally {
      setPackBusy(false);
    }
  };

  const confirmInstructionPack = async (draft: InstructionPackDraft) => {
    const accepted = await confirm({ title: `确认项目规则「${draft.manifest.name}」？`, description: '确认后可发布到本机规则库。发布不会自动启用规则，也不会改写客户端文件。', confirmText: '确认候选版本' });
    if (!accepted) return;
    setPackBusy(true);
    try {
      const next = await agentApi.confirmInstructionPackDraft(draft.manifest.id, draft.manifest.version);
      setPackDrafts(current => current.map(item => item.manifest.id === draft.manifest.id && item.manifest.version === draft.manifest.version ? next : item));
    } catch (reason) { setError(typeof reason === 'string' ? reason : '确认项目规则失败'); }
    finally { setPackBusy(false); }
  };

  const materialize = async (draft: InstructionPackDraft) => {
    if (!onMaterializeToWorkspace) return;
    setPackBusy(true);
    setError('');
    setPackMessage('');
    try {
      await onMaterializeToWorkspace(draft);
      setPackMessage(`已复制到工作区：${draft.manifest.name}`);
    } catch (reason) {
      setError(typeof reason === 'string' ? reason : '复制到工作区失败');
    } finally { setPackBusy(false); }
  };

  const publishInstructionPack = async (draft: InstructionPackDraft) => {
    setPackBusy(true);
    try {
      const next = await agentApi.publishInstructionPackLocally(draft.manifest.id, draft.manifest.version);
      setPackDrafts(current => current.map(item => item.manifest.id === draft.manifest.id && item.manifest.version === draft.manifest.version ? next : item));
      setPackMessage('已发布到本机规则库，可同步到 AI 客户端。');
      onLibraryChanged?.();
    } catch (reason) { setError(typeof reason === 'string' ? reason : '发布项目规则失败'); }
    finally { setPackBusy(false); }
  };

  useEffect(() => {
    if (open || embedded) void loadTargets();
  }, [workspaceRoot, open, embedded, clientsTab]);

  const plan = async (item: InstructionTargetDescriptor) => {
    const key = item.target.adapter_id;
    setBusyKey(key);
    setError('');
    try {
      const refs = (selectedPacks[key] || []).map(value => {
        const [id, version] = value.split('@');
        const draft = packDrafts.find(candidate => candidate.manifest.id === id && candidate.manifest.version === version);
        return { id, version, digest: draft?.published_digest || '' };
      });
      const result = await agentApi.planInstructionProjection(workspaceRoot, { ...item.target, instruction_packs: refs });
      setPlans(current => ({ ...current, [key]: result }));
    } catch (reason) {
      setError(typeof reason === 'string' ? reason : `${targetLabel(item)}计划生成失败`);
    } finally {
      setBusyKey('');
    }
  };

  const apply = async (item: InstructionTargetDescriptor, projectionPlan: InstructionProjectionPlan) => {
    const accepted = await confirm({
      title: `写入 ${targetLabel(item)}？`,
      description: 'HiMind 只会写入受管区块，并在写入前保留备份。',
      confirmText: '确认写入',
    });
    if (!accepted) return;
    const key = item.target.adapter_id;
    setBusyKey(key);
    setError('');
    try {
      const receipt = await agentApi.applyInstructionProjection(projectionPlan);
      setReceipts(current => ({ ...current, [key]: receipt }));
      await plan(item);
    } catch (reason) {
      setError(typeof reason === 'string' ? reason : `${targetLabel(item)}写入失败`);
    } finally {
      setBusyKey('');
    }
  };

  const rollback = async (item: InstructionTargetDescriptor, receipt: InstructionProjectionReceipt) => {
    const accepted = await confirm({
      title: `回滚 ${targetLabel(item)}？`,
      description: '目标文件在同步后被修改时，回滚会停止并保留当前内容。',
      confirmText: '回滚',
    });
    if (!accepted) return;
    const key = item.target.adapter_id;
    setBusyKey(key);
    setError('');
    try {
      await agentApi.rollbackInstructionProjection(receipt);
      setReceipts(current => {
        const next = { ...current };
        delete next[key];
        return next;
      });
      await plan(item);
    } catch (reason) {
      setError(typeof reason === 'string' ? reason : `${targetLabel(item)}回滚失败`);
    } finally {
      setBusyKey('');
    }
  };

  const detectedCount = targets.filter(item => item.detected).length;

  const ruleTabs =       <nav className="plugin-tabs instruction-panel-tabs" role="tablist" aria-label="项目规则">
        <button type="button" role="tab" aria-selected={activeView === 'rules'} className={activeView === 'rules' ? 'active' : ''} onClick={() => setActiveView('rules')}>规则库 <span>{packDrafts.length}</span></button>
        {clientsTab ? <button type="button" role="tab" aria-selected={activeView === 'clients'} className={activeView === 'clients' ? 'active' : ''} onClick={() => setActiveView('clients')}>同步到客户端 <span>{detectedCount}</span></button> : null}
      </nav>;
  const refreshTargets = <button type="button" className="btn btn-icon" title="重新读取客户端目标" aria-label="重新读取客户端目标" disabled={loading || disabled} onClick={() => void loadTargets()}><RefreshCw size={15} /></button>;
  const panelBody = <div className="instruction-panel-body">
    {embedded ? <div className="instruction-panel-embedded-head">{ruleTabs}{clientsTab ? <div className="instruction-panel-actions">{refreshTargets}</div> : null}</div> : ruleTabs}
      {loading ? <div className="instruction-panel-loading"><BusyIndicator size={15} />读取客户端规则目标</div> : null}
      {error ? <div className="instruction-panel-error"><CircleAlert size={15} /><span>{error}</span></div> : null}
      {packMessage ? <div className="instruction-panel-notice"><CheckCircle2 size={15} /><span>{packMessage}</span></div> : null}
      {activeView === 'rules' ? <div className="instruction-rule-library">
        <div className="instruction-section-intro">把项目说明、输出要求和团队约定保存下来，供 AI 客户端加载。</div>
      <div className="instruction-rule-toolbar">
        <button type="button" className="btn" disabled={packBusy || disabled} onClick={() => void importInstructionPack()}><PackagePlus size={14} />导入规则文件</button>
        <button type="button" className="btn" disabled={packBusy || disabled} onClick={() => void importInstructionPackage()}><PackagePlus size={14} />导入安装包</button>
        <button type="button" className="btn btn-primary" disabled={packBusy || disabled} onClick={() => setAuthoring(value => !value)}><FileCode2 size={14} />{authoring ? '收起编辑' : '新建项目规则'}</button>
      </div>
      {authoring ? <form className="instruction-pack-authoring" onSubmit={saveInstructionPack}>
        <div className="instruction-pack-authoring-head"><strong>新建项目规则</strong><small>保存为草稿后，完成检查、确认和发布。发布不会自动启用或写入客户端文件。</small></div>
        <div className="instruction-pack-authoring-grid">
          <label>名称<input required value={authoringInput.name} onChange={event => setAuthoringInput(current => ({ ...current, name: event.target.value }))} placeholder="例如：前端交付规范" /></label>
          <label>稳定 ID<input required value={authoringInput.id} onChange={event => setAuthoringInput(current => ({ ...current, id: event.target.value }))} /></label>
          <label>版本<input required value={authoringInput.version} onChange={event => setAuthoringInput(current => ({ ...current, version: event.target.value }))} /></label>
          <label>更新说明<input required value={authoringInput.release_notes} onChange={event => setAuthoringInput(current => ({ ...current, release_notes: event.target.value }))} /></label>
          <label className="instruction-pack-authoring-wide">简介<input required value={authoringInput.description} onChange={event => setAuthoringInput(current => ({ ...current, description: event.target.value }))} placeholder="这份规则适用于什么工作" /></label>
          <label className="instruction-pack-authoring-wide">规则内容<textarea required rows={7} value={authoringInput.instructions} onChange={event => setAuthoringInput(current => ({ ...current, instructions: event.target.value }))} /></label>
        </div>
        <div className="instruction-target-actions"><button type="submit" className="btn btn-primary" disabled={packBusy || disabled}>保存草稿</button></div>
      </form> : null}
      {packDrafts.length ? <div className="instruction-pack-drafts">
        <div className="instruction-section-head"><div><strong>规则版本</strong><small>发布后可在 HiMind 会话中选择，或同步到其他 AI 客户端</small></div><PackagePlus size={16} /></div>
        {packDrafts.slice(0, 5).map(draft => <article className="instruction-pack-draft" key={`${draft.manifest.id}:${draft.manifest.version}`}>
          <div><strong>{draft.manifest.name}</strong><small>v{draft.manifest.version} · {draft.source}{draft.published_at ? ' · 已发布' : draft.confirmed_at ? ' · 已确认' : draft.tested_at ? ' · 已检查' : ' · 草稿'}</small></div>
          <div className="instruction-target-actions">
            {!draft.tested_at ? <button type="button" className="btn" disabled={packBusy || disabled} onClick={() => void testInstructionPack(draft)}>检查</button> : null}
            {draft.tested_at && !draft.confirmed_at ? <button type="button" className="btn" disabled={packBusy || disabled} onClick={() => void confirmInstructionPack(draft)}>确认</button> : null}
            {draft.confirmed_at && !draft.published_at ? <button type="button" className="btn btn-primary" disabled={packBusy || disabled} onClick={() => void publishInstructionPack(draft)}>发布</button> : null}
            {onMaterializeToWorkspace ? <button type="button" className="btn" title="在工作区里生成一份可编辑的项目规则项目" disabled={packBusy || disabled} onClick={() => void materialize(draft)}><FolderPlus size={14} />复制到工作区</button> : null}
          </div>
        </article>)}
      </div> : !loading ? <div className="instruction-panel-empty">还没有项目规则。可以新建一份，或从文件、市场导入。</div> : null}
      </div> : null}
      {activeView === 'clients' ? <>
      <div className="instruction-section-intro">为每个客户端选择要同步的项目规则，再预览变更。HiMind 只管理标记区块，保留文件中的其他内容。</div>
      {!loading && !targets.length && workspaceRoot ? <div className="instruction-panel-empty">当前工作区没有可发现的客户端规则目标。</div> : null}
      <div className="instruction-target-list">
        {targets.map(item => {
          const key = item.target.adapter_id;
          const projectionPlan = plans[key];
          const receipt = receipts[key];
          const working = busyKey === key;
          return <article key={key} className={`instruction-target ${item.degraded ? 'is-degraded' : ''}`}>
            <div className="instruction-target-main">
              <span className="instruction-target-mark"><FileCode2 size={15} /></span>
              <div><strong>{targetLabel(item)}</strong><small>{targetState(item)} · {item.target.scope} · <code title={item.target.path}>{item.target.path}</code></small></div>
            </div>
            <Pill kind={item.degraded ? 'warn' : item.receipt ? 'success' : item.detected ? 'success' : 'neutral'}>{item.degraded ? '兼容' : item.receipt ? '已同步' : item.detected ? '已发现' : '未发现'}</Pill>
            <div className="instruction-target-actions">
              <button type="button" className="btn" disabled={working || disabled} onClick={() => void plan(item)}>{working ? <BusyIndicator size={13} /> : <FileCode2 size={13} />}预览</button>
              {projectionPlan?.writes.length ? <button type="button" className="btn btn-primary" disabled={working || disabled || Boolean(projectionPlan.conflicts.length) || Boolean(projectionPlan.unsupported.length)} onClick={() => void apply(item, projectionPlan)}><Upload size={13} />确认写入</button> : null}
              {receipt ? <button type="button" className="btn btn-danger-quiet" disabled={working || disabled} onClick={() => void rollback(item, receipt)}><RotateCcw size={13} />回滚</button> : null}
            </div>
            {projectionPlan ? <div className={`instruction-plan ${projectionPlan.status === 'conflict' || projectionPlan.status === 'blocked' ? 'is-danger' : ''}`}>
              <span><strong>{statusLabel(projectionPlan.status)}</strong> · {projectionPlan.writes.length ? `${projectionPlan.writes.length} 项待写入` : '无待写入项'}</span>
              {projectionPlan.conflicts.length ? <small>{projectionPlan.conflicts.join('；')}</small> : null}
              {projectionPlan.unsupported.length ? <small>{projectionPlan.unsupported.join('；')}</small> : null}
            </div> : null}
          </article>;
        })}
      </div>
      {packDrafts.some(item => item.published_at) ? <div className="instruction-pack-select">
        <div className="instruction-section-head"><div><strong>选择同步规则</strong><small>只可同步已发布的规则；选择后预览客户端文件。</small></div><ShieldCheck size={16} /></div>
        {targets.map(item => {
          const key = item.target.adapter_id;
          const values = selectedPacks[key] || [];
          return <div className="instruction-pack-select-row" key={key}><strong>{targetLabel(item)}</strong><div>{packDrafts.filter(draft => draft.published_at).map(draft => {
            const value = `${draft.manifest.id}@${draft.manifest.version}`;
            return <label key={value}><input type="checkbox" checked={values.includes(value)} disabled={disabled || packBusy} onChange={event => setSelectedPacks(current => ({ ...current, [key]: event.target.checked ? [...values, value] : values.filter(itemValue => itemValue !== value) }))} /><span>{draft.manifest.name} v{draft.manifest.version}</span></label>;
          })}</div></div>;
        })}
      </div> : <div className="instruction-panel-empty">先在「规则库」发布项目规则，再回来选择要同步的内容。</div>}
      </> : null}



    </div>;

  if (embedded) return <section className="instruction-panel is-embedded is-open">{panelBody}</section>;

  return <section className={`instruction-panel ${open ? 'is-open' : ''}`}>
    <div className="instruction-panel-head">
      <button type="button" className="instruction-panel-toggle" aria-expanded={open} onClick={() => setOpen(value => !value)}>
        <span className="instruction-panel-icon"><ShieldCheck size={16} /></span>
        <span className="instruction-panel-title"><strong>项目规则</strong><small>{workspaceRoot ? '设置项目说明，并按需同步到其他 AI 客户端' : '选择项目后管理项目规则'}</small></span>
      </button>
      <div className="instruction-panel-actions">
        {open ? refreshTargets : null}
      </div>
    </div>
    {open ? panelBody : null}
  </section>;
}
